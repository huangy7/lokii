import Cocoa
import XCTest
@testable import LokiiAppLib

final class FilterQuerySyncCoordinatorTests: XCTestCase {
    private final class MockTextViewWithMarkedText: NSTextView {
        var mockMarked = true
        override func hasMarkedText() -> Bool {
            mockMarked
        }
    }

    private final class MockSearchField: NSSearchField {
        let mockEditor = MockTextViewWithMarkedText()
        override func currentEditor() -> NSText? {
            mockEditor
        }
    }

    func testFilterStatePropertiesAndClearAll() {
        var state = FilterState()
        XCTAssertTrue(state.isUnconstrained)
        XCTAssertEqual(state.activeCount, 0)

        state.selectedTypes = [.code, .document]
        XCTAssertFalse(state.isUnconstrained)
        XCTAssertEqual(state.activeCount, 1)

        state.minSizeBytes = 10_000_000
        XCTAssertEqual(state.activeCount, 2)

        state.clearAll()
        XCTAssertTrue(state.isUnconstrained)
        XCTAssertEqual(state.activeCount, 0)
        XCTAssertTrue(state.selectedTypes.isEmpty)
        XCTAssertNil(state.minSizeBytes)
        XCTAssertNil(state.maxSizeBytes)

        state.selectedTypes = [.all]
        XCTAssertTrue(state.isUnconstrained)
        XCTAssertEqual(state.activeCount, 0)

        state.selectedTypes = [.document, .code, .image, .media, .archive, .folder]
        XCTAssertTrue(state.isUnconstrained)
        XCTAssertEqual(state.activeCount, 0)
    }

    func testSerializeAndDeserialize() {
        var state = FilterState()
        state.selectedTypes = [.code, .document]
        state.minSizeBytes = 10_000_000

        let result1 = FilterQuerySyncCoordinator.serialize(keyword: "lokii", state: state)
        XCTAssertTrue(result1.contains("lokii"))
        XCTAssertTrue(result1.contains("type:code,doc"))
        XCTAssertTrue(result1.contains("size:>10m"))

        XCTAssertEqual(FilterQuerySyncCoordinator.serialize(keyword: "", state: state), "type:code,doc size:>10m")

        var intervalState = FilterState()
        intervalState.minSizeBytes = 10_000_000
        intervalState.maxSizeBytes = 50_000_000
        XCTAssertEqual(FilterQuerySyncCoordinator.serialize(keyword: "archive", state: intervalState), "archive size:10m..50m")

        var maxOnlyState = FilterState()
        maxOnlyState.maxSizeBytes = 500_000
        XCTAssertEqual(FilterQuerySyncCoordinator.serialize(keyword: "doc", state: maxOnlyState), "doc size:<500k")

        XCTAssertEqual(FilterQuerySyncCoordinator.serialize(keyword: "hello world", state: FilterState()), "hello world")

        let (keyword1, state1) = FilterQuerySyncCoordinator.deserialize(query: "myproject type:code,doc size:>10m")
        XCTAssertEqual(keyword1, "myproject")
        XCTAssertEqual(state1.selectedTypes, [.code, .document])
        XCTAssertEqual(state1.minSizeBytes, 10_000_000)
        XCTAssertNil(state1.maxSizeBytes)

        let (keywordInterval, stateInterval) = FilterQuerySyncCoordinator.deserialize(query: "size:10m..50m lokii")
        XCTAssertEqual(keywordInterval, "lokii")
        XCTAssertEqual(stateInterval.minSizeBytes, 10_000_000)
        XCTAssertEqual(stateInterval.maxSizeBytes, 50_000_000)

        let (keywordMax, stateMax) = FilterQuerySyncCoordinator.deserialize(query: "report size:<500k")
        XCTAssertEqual(keywordMax, "report")
        XCTAssertNil(stateMax.minSizeBytes)
        XCTAssertEqual(stateMax.maxSizeBytes, 500_000)

        let (_, stateUpper) = FilterQuerySyncCoordinator.deserialize(query: "TYPE:CODE,DOC SIZE:>10M")
        XCTAssertEqual(stateUpper.selectedTypes, [.code, .document])
        XCTAssertEqual(stateUpper.minSizeBytes, 10_000_000)

        let (kwEmpty, stateEmpty) = FilterQuerySyncCoordinator.deserialize(query: "")
        XCTAssertEqual(kwEmpty, "")
        XCTAssertTrue(stateEmpty.isUnconstrained)

        let (kwSpaces, stateSpaces) = FilterQuerySyncCoordinator.deserialize(query: "    ")
        XCTAssertEqual(kwSpaces, "")
        XCTAssertTrue(stateSpaces.isUnconstrained)

        let (kwPureSyntax, statePureSyntax) = FilterQuerySyncCoordinator.deserialize(query: "type:code size:>10m")
        XCTAssertEqual(kwPureSyntax, "")
        XCTAssertEqual(statePureSyntax.selectedTypes, [.code])
        XCTAssertEqual(statePureSyntax.minSizeBytes, 10_000_000)

        let (kwPlain, statePlain) = FilterQuerySyncCoordinator.deserialize(query: "swift build release tool")
        XCTAssertEqual(kwPlain, "swift build release tool")
        XCTAssertTrue(statePlain.isUnconstrained)

        let (kwInvalidType, stateInvalidType) = FilterQuerySyncCoordinator.deserialize(query: "type:unknowncategory mycode")
        XCTAssertEqual(kwInvalidType, "type:unknowncategory mycode")
        XCTAssertTrue(stateInvalidType.selectedTypes.isEmpty)

        let (kwInvalidSize, stateInvalidSize) = FilterQuerySyncCoordinator.deserialize(query: "size:notanumber report")
        XCTAssertEqual(kwInvalidSize, "size:notanumber report")
        XCTAssertNil(stateInvalidSize.minSizeBytes)
    }

    func testReentrancyLockAndMarkedTextProtection() {
        let coordinator = FilterQuerySyncCoordinator()
        let searchField = NSSearchField()
        searchField.stringValue = "initial query"

        var searchPerformedWith: String?
        var state = FilterState()
        state.selectedTypes = [.code]
        coordinator.updateQueryFromUI(state: state, searchField: searchField) { newQuery in
            searchPerformedWith = newQuery
        }
        XCTAssertEqual(searchField.stringValue, "initial query type:code ")
        XCTAssertEqual(searchPerformedWith, "initial query type:code ")

        var loopTriggerCount = 0
        coordinator.updateQueryFromUI(state: state, searchField: searchField) { _ in
            coordinator.updateUIFromQuery(query: "nested query") { _ in
                loopTriggerCount += 1
            }
        }
        XCTAssertEqual(loopTriggerCount, 0)

        var receivedState: FilterState?
        coordinator.updateUIFromQuery(query: "lokii type:media size:<1g") { s in
            receivedState = s
        }
        XCTAssertEqual(receivedState?.selectedTypes, [.media])
        XCTAssertEqual(receivedState?.maxSizeBytes, 1_000_000_000)

        let markedSearchField = MockSearchField()
        markedSearchField.stringValue = "拼音输入zhong"
        var markedSearchTriggered = false

        var newState = FilterState()
        newState.selectedTypes = [.document]
        coordinator.updateQueryFromUI(state: newState, searchField: markedSearchField) { _ in
            markedSearchTriggered = true
        }
        XCTAssertFalse(markedSearchTriggered)
        XCTAssertEqual(markedSearchField.stringValue, "拼音输入zhong")
        XCTAssertEqual(coordinator.pendingState, newState)

        XCTAssertFalse(coordinator.flushPendingStateIfNeeded(searchField: markedSearchField) { _ in })
        XCTAssertEqual(coordinator.pendingState, newState)

        var newerState = FilterState()
        newerState.selectedTypes = [.code, .document]
        coordinator.updateQueryFromUI(state: newerState, searchField: markedSearchField) { _ in }
        XCTAssertEqual(coordinator.pendingState, newerState)

        markedSearchField.mockEditor.mockMarked = false
        XCTAssertTrue(coordinator.flushPendingStateIfNeeded(searchField: markedSearchField) { _ in
            markedSearchTriggered = true
        })
        XCTAssertNil(coordinator.pendingState)
        XCTAssertTrue(markedSearchTriggered)
        XCTAssertEqual(markedSearchField.stringValue, "拼音输入zhong type:code,doc ")
        XCTAssertEqual(markedSearchField.mockEditor.selectedRange.location, ("拼音输入zhong type:code,doc " as NSString).length)
        XCTAssertEqual(markedSearchField.mockEditor.selectedRange.length, 0)
        XCTAssertTrue(markedSearchField.mockEditor.layoutManager is PillLayoutManager)
        XCTAssertNil(markedSearchField.mockEditor.typingAttributes[.backgroundColor])

        XCTAssertFalse(coordinator.flushPendingStateIfNeeded(searchField: markedSearchField) { _ in })
    }

    func testFormatQuerySyntaxAndQuotedIsolation() {
        XCTAssertEqual(FilterQuerySyncCoordinator.formatQuerySyntax("").length, 0)

        let query = "readme type:code,doc size:>10m ext:swift"
        let attrStr = FilterQuerySyncCoordinator.formatQuerySyntax(query)
        XCTAssertEqual(attrStr.string, query)
        let nsQuery = query as NSString

        let readmeRange = nsQuery.range(of: "readme")
        XCTAssertNil(attrStr.attributes(at: readmeRange.location, effectiveRange: nil)[.backgroundColor])

        let typeKeyRange = nsQuery.range(of: "type:")
        XCTAssertEqual(attrStr.attributes(at: typeKeyRange.location, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.secondaryLabelColor)

        let typeValRange = nsQuery.range(of: "code,doc")
        let typeValAttrs = attrStr.attributes(at: typeValRange.location, effectiveRange: nil)
        XCTAssertEqual(typeValAttrs[.foregroundColor] as? NSColor, NSColor.controlAccentColor)
        XCTAssertNotNil(typeValAttrs[.backgroundColor])

        let sizeKeyRange = nsQuery.range(of: "size:")
        let sizeValRange = nsQuery.range(of: ">10m")
        XCTAssertEqual(attrStr.attributes(at: sizeKeyRange.location, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.secondaryLabelColor)
        XCTAssertEqual(attrStr.attributes(at: sizeValRange.location, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.controlAccentColor)
        XCTAssertNotNil(attrStr.attributes(at: sizeValRange.location, effectiveRange: nil)[.backgroundColor])

        let extKeyRange = nsQuery.range(of: "ext:")
        let extValRange = nsQuery.range(of: "swift")
        XCTAssertEqual(attrStr.attributes(at: extKeyRange.location, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.secondaryLabelColor)
        XCTAssertEqual(attrStr.attributes(at: extValRange.location, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.controlAccentColor)
        XCTAssertNotNil(attrStr.attributes(at: extValRange.location, effectiveRange: nil)[.backgroundColor])

        let incompleteAttr = FilterQuerySyncCoordinator.formatQuerySyntax("type:")
        XCTAssertEqual(incompleteAttr.attributes(at: 0, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.secondaryLabelColor)

        let upperQuery = "TYPE:CODE"
        let upperAttr = FilterQuerySyncCoordinator.formatQuerySyntax(upperQuery)
        let codeRange = (upperQuery as NSString).range(of: "CODE")
        XCTAssertEqual(upperAttr.attributes(at: codeRange.location, effectiveRange: nil)[.foregroundColor] as? NSColor, NSColor.controlAccentColor)

        for sample in ["\"type:doc\"", "\"ext:rs\"", "\"size:>1mb\"", "!\"type:doc\""] {
            let (kw, state) = FilterQuerySyncCoordinator.deserialize(query: sample)
            XCTAssertEqual(kw, sample)
            XCTAssertTrue(state.isUnconstrained)

            let attr = FilterQuerySyncCoordinator.formatQuerySyntax(sample)
            let nsSample = sample as NSString
            for i in 0..<nsSample.length {
                let attrs = attr.attributes(at: i, effectiveRange: nil)
                XCTAssertNil(attrs[.backgroundColor])
                XCTAssertEqual(attrs[.foregroundColor] as? NSColor, NSColor.labelColor)
            }
        }

        let mixed = "\"hello type:doc\" size:>10m ext:rs"
        let (mixedKw, mixedState) = FilterQuerySyncCoordinator.deserialize(query: mixed)
        XCTAssertEqual(mixedKw, "\"hello type:doc\" ext:rs")
        XCTAssertEqual(mixedState.selectedTypes, [])
        XCTAssertEqual(mixedState.minSizeBytes, 10_000_000)

        let mixedAttr = FilterQuerySyncCoordinator.formatQuerySyntax(mixed)
        let nsMixed = mixed as NSString
        XCTAssertNil(mixedAttr.attributes(at: nsMixed.range(of: "type:doc").location, effectiveRange: nil)[.backgroundColor])
        XCTAssertNotNil(mixedAttr.attributes(at: nsMixed.range(of: ">10m").location, effectiveRange: nil)[.backgroundColor])
        XCTAssertNotNil(mixedAttr.attributes(at: nsMixed.range(of: "rs", options: .backwards).location, effectiveRange: nil)[.backgroundColor])

        let invalidTokensQuery = "type:pdf size:notanumber report"
        let invalidAttr = FilterQuerySyncCoordinator.formatQuerySyntax(invalidTokensQuery)
        let nsInvalid = invalidTokensQuery as NSString
        for idx in 0..<nsInvalid.length {
            let attrs = invalidAttr.attributes(at: idx, effectiveRange: nil)
            XCTAssertNil(attrs[.backgroundColor])
            XCTAssertEqual(attrs[.foregroundColor] as? NSColor, NSColor.labelColor)
        }
    }
}
