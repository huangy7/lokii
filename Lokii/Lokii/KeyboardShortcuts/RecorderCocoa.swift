#if os(macOS)
import AppKit
import Carbon.HIToolbox

extension KeyboardShortcuts {
	/**
	macOS 原生单体胶囊式全局快捷键录制控件（方案 A 标准原生按钮风格）。
	彻底消除多层卡片小方块嵌套，回归纯正 macOS 原生胶囊质感。
	支持全域点击（空白区域或自身 Toggle）安全失焦退出与按键实时监听，彻底屏蔽系统输入法上下文。
	*/
	public final class RecorderCocoa: NSControl {
		private let controlHeight = 24.0
		private let onChange: ((_ shortcut: Shortcut?) -> Void)?
		private var canBecomeKey = false
		private var isHovered = false
		private var shortcutsNameChangeObserver: NSObjectProtocol?
		private var windowDidResignKeyObserver: NSObjectProtocol?
		private var windowDidBecomeKeyObserver: NSObjectProtocol?
		private var mouseEventMonitor: Any?
		private var trackingArea: NSTrackingArea?

		private var labelTrailingToClearConstraint: NSLayoutConstraint?
		private var labelTrailingToSelfConstraint: NSLayoutConstraint?

		public private(set) var isRecording = false
		private var currentModifiers: NSEvent.ModifierFlags = []
		private var temporaryNotice: String?

		/**
		强类型快捷键名称标识
		*/
		public var shortcutName: Name {
			didSet {
				guard shortcutName != oldValue else { return }
				updateUI()
			}
		}

		/// 屏蔽系统输入法与 TSM 文本上下文，彻底杜绝打出拼音与字符
		override public var inputContext: NSTextInputContext? { nil }

		override public var acceptsFirstResponder: Bool { canBecomeKey }
		override public var canBecomeKeyView: Bool { canBecomeKey }
		override public var focusRingType: NSFocusRingType {
			get { .none }
			set { /* 强制禁用粗糙默认外环，统一采用单体内建强调色微光边框 */ }
		}

		private var hasShortcut: Bool {
			getShortcut(for: shortcutName) != nil
		}

		private let displayLabel: NSTextField = {
			let label = NSTextField(labelWithString: "")
			label.translatesAutoresizingMaskIntoConstraints = false
			label.alignment = .center
			label.lineBreakMode = .byClipping
			label.isSelectable = false
			label.isEditable = false
			label.isBezeled = false
			label.drawsBackground = false
			label.setContentHuggingPriority(.required, for: .horizontal)
			label.setContentCompressionResistancePriority(.required, for: .horizontal)
			return label
		}()

		private lazy var clearButton: NSButton = {
			let button = NSButton()
			button.translatesAutoresizingMaskIntoConstraints = false
			button.isBordered = false
			button.bezelStyle = .regularSquare
			button.setButtonType(.momentaryChange)
			let config = NSImage.SymbolConfiguration(pointSize: 12, weight: .regular)
			button.image = NSImage(systemSymbolName: "xmark.circle.fill", accessibilityDescription: "shortcut.clear".localized)?
				.withSymbolConfiguration(config)
			button.contentTintColor = .tertiaryLabelColor
			button.toolTip = "shortcut.clear".localized
			button.target = self
			button.action = #selector(clearButtonClicked)
			return button
		}()

		public required init(
			for name: Name,
			onChange: ((_ shortcut: Shortcut?) -> Void)? = nil
		) {
			self.shortcutName = name
			self.onChange = onChange
			super.init(frame: NSRect(x: 0, y: 0, width: 80, height: controlHeight))

			setupUI()
			setupEvents()
			updateUI()
		}

		@available(*, unavailable)
		public required init?(coder: NSCoder) {
			fatalError("init(coder:) has not been implemented")
		}

		deinit {
			removeEventMonitor()
			if let observer = shortcutsNameChangeObserver {
				NotificationCenter.default.removeObserver(observer)
			}
			if let observer = windowDidResignKeyObserver {
				NotificationCenter.default.removeObserver(observer)
			}
			if let observer = windowDidBecomeKeyObserver {
				NotificationCenter.default.removeObserver(observer)
			}
		}

		private func setupUI() {
			wantsLayer = true
			layer?.cornerRadius = 5
			layer?.masksToBounds = true

			setContentHuggingPriority(.required, for: .horizontal)
			setContentCompressionResistancePriority(.required, for: .horizontal)

			addSubview(displayLabel)
			addSubview(clearButton)

			let centerYConstraint = displayLabel.centerYAnchor.constraint(equalTo: centerYAnchor)
			let leadingConstraint = displayLabel.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8)
			let heightConstraint = heightAnchor.constraint(equalToConstant: controlHeight)
			let minWidthConstraint = widthAnchor.constraint(greaterThanOrEqualToConstant: 76)

			let clearCenter = clearButton.centerYAnchor.constraint(equalTo: centerYAnchor)
			let clearTrailing = clearButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -6)
			let clearW = clearButton.widthAnchor.constraint(equalToConstant: 13)
			let clearH = clearButton.heightAnchor.constraint(equalToConstant: 13)

			labelTrailingToClearConstraint = clearButton.leadingAnchor.constraint(equalTo: displayLabel.trailingAnchor, constant: 4)
			labelTrailingToSelfConstraint = trailingAnchor.constraint(equalTo: displayLabel.trailingAnchor, constant: 8)

			NSLayoutConstraint.activate([
				centerYConstraint,
				leadingConstraint,
				heightConstraint,
				minWidthConstraint,
				clearCenter,
				clearTrailing,
				clearW,
				clearH,
			])
		}

		override public var intrinsicContentSize: NSSize {
			let labelSize = displayLabel.intrinsicContentSize
			let clearWidth: CGFloat = (hasShortcut && !isRecording) ? 19 : 0
			let width = max(76, labelSize.width + 16 + clearWidth)
			return NSSize(width: width, height: controlHeight)
		}

		private func setupEvents() {
			shortcutsNameChangeObserver = NotificationCenter.default.addObserver(
				forName: .shortcutByNameDidChange,
				object: nil,
				queue: nil
			) { [weak self] notification in
				guard
					let self,
					let nameInNotification = notification.userInfo?["name"] as? KeyboardShortcuts.Name,
					nameInNotification == self.shortcutName
				else {
					return
				}

				self.updateUI()
			}
		}

		private func updateUI() {
			guard let layer else { return }

			let shouldShowClear = hasShortcut && !isRecording

			if shouldShowClear {
				labelTrailingToSelfConstraint?.isActive = false
				labelTrailingToClearConstraint?.isActive = true
				clearButton.isHidden = false
			} else {
				labelTrailingToClearConstraint?.isActive = false
				labelTrailingToSelfConstraint?.isActive = true
				clearButton.isHidden = true
			}

			let isDark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua

			// 单体原生按钮质感调色（彻底消灭多层小框嵌套）
			if isRecording {
				layer.borderColor = NSColor.controlAccentColor.cgColor
				layer.borderWidth = 1.5
				layer.backgroundColor = NSColor.controlAccentColor.withAlphaComponent(isDark ? 0.15 : 0.08).cgColor
			} else {
				if isDark {
					layer.borderColor = NSColor(white: 1.0, alpha: 0.15).cgColor
					layer.borderWidth = 0.5
					let baseAlpha: CGFloat = isHovered ? 0.18 : 0.10
					layer.backgroundColor = NSColor(white: 1.0, alpha: baseAlpha).cgColor
				} else {
					layer.borderColor = NSColor.separatorColor.withAlphaComponent(0.35).cgColor
					layer.borderWidth = 0.5
					let baseAlpha: CGFloat = isHovered ? 0.12 : 0.06
					layer.backgroundColor = NSColor.quaternaryLabelColor.withAlphaComponent(baseAlpha).cgColor
				}
			}

			// 单体文本格式化与排版渲染
			if let notice = temporaryNotice {
				displayLabel.stringValue = notice
				displayLabel.font = .systemFont(ofSize: 10, weight: .medium)
				displayLabel.textColor = .systemOrange
			} else if isRecording {
				displayLabel.font = .systemFont(ofSize: 11, weight: .medium)
				displayLabel.textColor = .controlAccentColor
				if currentModifiers.isEmpty {
					displayLabel.stringValue = "press_shortcut".localized
				} else {
					let symbols = modifierSymbols(from: currentModifiers)
					displayLabel.stringValue = symbols.joined(separator: " ") + " …"
				}
			} else if let shortcut = getShortcut(for: shortcutName) {
				displayLabel.font = .systemFont(ofSize: 11, weight: .medium)
				displayLabel.textColor = .labelColor
				displayLabel.stringValue = formattedShortcutString(for: shortcut)
			} else {
				displayLabel.font = .systemFont(ofSize: 11, weight: .regular)
				displayLabel.textColor = .secondaryLabelColor
				displayLabel.stringValue = "record_shortcut".localized
			}

			invalidateIntrinsicContentSize()
		}

		override public func updateLayer() {
			super.updateLayer()
			updateUI()
		}

		private func formattedShortcutString(for shortcut: Shortcut) -> String {
			let symbols = modifierSymbols(from: shortcut.modifiers)
			let keyTitle = extractKeyTitle(from: shortcut)
			if keyTitle.isEmpty {
				return symbols.joined(separator: " ")
			}
			if symbols.isEmpty {
				return keyTitle
			}
			return symbols.joined(separator: " ") + " " + keyTitle
		}

		private func modifierSymbols(from modifiers: NSEvent.ModifierFlags) -> [String] {
			var symbols: [String] = []
			if modifiers.contains(.control) { symbols.append("⌃") }
			if modifiers.contains(.option) { symbols.append("⌥") }
			if modifiers.contains(.shift) { symbols.append("⇧") }
			if modifiers.contains(.command) { symbols.append("⌘") }
			return symbols
		}

		private func extractKeyTitle(from shortcut: Shortcut) -> String {
			let fullDesc = "\(shortcut)"
			let modStr = shortcut.modifiers.ks_symbolicRepresentation
			if fullDesc.hasPrefix(modStr) {
				let keyPart = String(fullDesc.dropFirst(modStr.count))
				return keyPart.isEmpty ? fullDesc : keyPart
			}
			return fullDesc
		}

		// MARK: - 录制生命周期与外部点击失焦监听（Local Event Monitor）

		private func startRecording() {
			guard !isRecording else { return }
			isRecording = true
			currentModifiers = []
			temporaryNotice = nil
			removeEventMonitor()

			// 注册本地鼠标监视器：支持点击界面任意空白/外部区域直接取消，点击自身 Toggle 取消
			mouseEventMonitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown]) { [weak self] event in
				guard let self, self.isRecording else { return event }

				guard let window = self.window, event.window == window else {
					self.endRecording(cancelled: true)
					self.window?.makeFirstResponder(nil)
					return event
				}

				let locationInView = self.convert(event.locationInWindow, from: nil)

				// 点击自身：Toggle 取消录制，并吞掉事件以阻断 mouseDown 重新唤醒
				if self.bounds.contains(locationInView) {
					self.endRecording(cancelled: true)
					self.window?.makeFirstResponder(nil)
					return nil
				}

				// 点击外部任意区域：平滑退出录制，并透传事件让外部控件正常响应
				self.endRecording(cancelled: true)
				self.window?.makeFirstResponder(nil)
				return event
			}

			KeyboardShortcuts.isPaused = true
			NotificationCenter.default.post(
				name: .recorderActiveStatusDidChange,
				object: nil,
				userInfo: ["isActive": true]
			)
			updateUI()
		}

		private func endRecording(cancelled: Bool) {
			guard isRecording else { return }
			isRecording = false
			currentModifiers = []
			temporaryNotice = nil
			removeEventMonitor()
			KeyboardShortcuts.isPaused = false
			NotificationCenter.default.post(
				name: .recorderActiveStatusDidChange,
				object: nil,
				userInfo: ["isActive": false]
			)
			updateUI()
		}

		private func removeEventMonitor() {
			guard let monitor = mouseEventMonitor else { return }
			mouseEventMonitor = nil
			DispatchQueue.main.async {
				NSEvent.removeMonitor(monitor)
			}
		}

		override public func becomeFirstResponder() -> Bool {
			let result = super.becomeFirstResponder()
			if result {
				startRecording()
			}
			return result
		}

		override public func resignFirstResponder() -> Bool {
			endRecording(cancelled: true)
			return super.resignFirstResponder()
		}

		// MARK: - 原生键盘事件处理（无 LocalEventMonitor 释放后重用崩溃风险）

		override public func flagsChanged(with event: NSEvent) {
			guard isRecording else {
				super.flagsChanged(with: event)
				return
			}

			let modifiers = event.modifierFlags.intersection([.command, .option, .control, .shift])
			currentModifiers = modifiers
			updateUI()
		}

		override public func performKeyEquivalent(with event: NSEvent) -> Bool {
			guard isRecording else {
				return super.performKeyEquivalent(with: event)
			}
			return processKeyEvent(event)
		}

		override public func keyDown(with event: NSEvent) {
			guard isRecording else {
				super.keyDown(with: event)
				return
			}
			_ = processKeyEvent(event)
		}

		@discardableResult
		private func processKeyEvent(_ event: NSEvent) -> Bool {
			if event.keyCode == kVK_Escape {
				endRecording(cancelled: true)
				window?.makeFirstResponder(nil)
				return true
			}

			let activeModifiers = event.modifierFlags.intersection([.command, .option, .control, .shift])

			if (event.keyCode == kVK_Delete || event.keyCode == kVK_ForwardDelete), activeModifiers.isEmpty {
				saveShortcut(nil)
				endRecording(cancelled: false)
				window?.makeFirstResponder(nil)
				return true
			}

			if event.keyCode == kVK_Tab, activeModifiers.isEmpty {
				endRecording(cancelled: true)
				window?.selectNextKeyView(nil)
				return true
			}

			guard !activeModifiers.isEmpty || event.specialKey?.isFunctionKey == true else {
				showModifierRequiredNotice()
				return true
			}

			guard let shortcut = Shortcut(event: event) else {
				showModifierRequiredNotice()
				return true
			}

			// 检查主菜单冲突
			if let menuItem = shortcut.takenByMainMenu {
				let hostWindow = self.window
				DispatchQueue.main.async {
					guard let hostWindow else { return }
					let alert = NSAlert()
					alert.messageText = String.localizedStringWithFormat("keyboard_shortcut_used_by_menu_item".localized, menuItem.title)
					alert.alertStyle = .warning
					alert.beginSheetModal(for: hostWindow, completionHandler: nil)
				}
				endRecording(cancelled: true)
				window?.makeFirstResponder(nil)
				return true
			}

			// 检查系统沙盒限制
			if shortcut.isDisallowed {
				let hostWindow = self.window
				DispatchQueue.main.async {
					guard let hostWindow else { return }
					let alert = NSAlert()
					alert.messageText = "keyboard_shortcut_disallowed".localized
					alert.alertStyle = .warning
					alert.beginSheetModal(for: hostWindow, completionHandler: nil)
				}
				endRecording(cancelled: true)
				window?.makeFirstResponder(nil)
				return true
			}

			// 保存快捷键并生效
			saveShortcut(shortcut)
			endRecording(cancelled: false)
			window?.makeFirstResponder(nil)
			return true
		}

		private func showModifierRequiredNotice() {
			temporaryNotice = "shortcut.modifierRequired".localized
			updateUI()
			shakeAnimation()
			DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { [weak self] in
				guard let self, self.isRecording else { return }
				self.temporaryNotice = nil
				self.updateUI()
			}
		}

		private func shakeAnimation() {
			let animation = CAKeyframeAnimation(keyPath: "transform.translation.x")
			animation.timingFunction = CAMediaTimingFunction(name: .linear)
			animation.duration = 0.25
			animation.values = [-3, 3, -2, 2, -1, 1, 0]
			layer?.add(animation, forKey: "shake")
		}

		@objc private func clearButtonClicked() {
			saveShortcut(nil)
			endRecording(cancelled: false)
		}

		private func saveShortcut(_ shortcut: Shortcut?) {
			setShortcut(shortcut, for: shortcutName)
			onChange?(shortcut)
			updateUI()
		}

		override public func mouseDown(with event: NSEvent) {
			guard isEnabled else { return }
			let location = convert(event.locationInWindow, from: nil)
			if clearButton.frame.contains(location) && hasShortcut && !isRecording {
				clearButtonClicked()
				return
			}
			if !isRecording {
				window?.makeFirstResponder(self)
			}
		}

		override public func viewDidMoveToWindow() {
			guard let window else {
				windowDidResignKeyObserver = nil
				windowDidBecomeKeyObserver = nil
				endRecording(cancelled: true)
				removeEventMonitor()
				return
			}

			windowDidResignKeyObserver = NotificationCenter.default.addObserver(
				forName: NSWindow.didResignKeyNotification,
				object: window,
				queue: nil
			) { [weak self] _ in
				self?.window?.makeFirstResponder(nil)
			}

			windowDidBecomeKeyObserver = NotificationCenter.default.addObserver(
				forName: NSWindow.didBecomeKeyNotification,
				object: window,
				queue: nil
			) { [weak self] _ in
				self?.preventBecomingKey()
			}

			preventBecomingKey()
		}

		private func preventBecomingKey() {
			canBecomeKey = false
			DispatchQueue.main.async { [weak self] in
				self?.canBecomeKey = true
			}
		}

		override public func updateTrackingAreas() {
			super.updateTrackingAreas()
			if let trackingArea {
				removeTrackingArea(trackingArea)
			}
			let area = NSTrackingArea(
				rect: bounds,
				options: [.mouseEnteredAndExited, .activeAlways],
				owner: self,
				userInfo: nil
			)
			addTrackingArea(area)
			self.trackingArea = area
		}

		override public func mouseEntered(with event: NSEvent) {
			guard !isRecording else { return }
			isHovered = true
			clearButton.contentTintColor = .secondaryLabelColor
			updateUI()
		}

		override public func mouseExited(with event: NSEvent) {
			guard !isRecording else { return }
			isHovered = false
			clearButton.contentTintColor = .tertiaryLabelColor
			updateUI()
		}

		override public func resetCursorRects() {
			super.resetCursorRects()
			addCursorRect(bounds, cursor: .pointingHand)
		}
	}
}

extension Notification.Name {
	static let recorderActiveStatusDidChange = Self("KeyboardShortcuts_recorderActiveStatusDidChange")
}
#endif
