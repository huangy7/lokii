import Foundation
import PDFKit
import Vision
import ImageIO
import CoreGraphics
import Darwin

private struct HelperOutput: Encodable {
    let ok: Bool
    let pages: UInt32
    let text: String?
    let error: String?
}

private func emit(_ output: HelperOutput, exitCode: Int32 = 0) -> Never {
    if let data = try? JSONEncoder().encode(output) {
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data("\n".utf8))
    }
    Darwin.exit(exitCode)
}

private func recognizeText(in cgImage: CGImage) -> String {
    var recognizedLines: [String] = []
    let request = VNRecognizeTextRequest { req, _ in
        guard let observations = req.results as? [VNRecognizedTextObservation] else { return }
        for obs in observations {
            if let top = obs.topCandidates(1).first {
                recognizedLines.append(top.string)
            }
        }
    }
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = true
    request.recognitionLanguages = ["zh-Hans", "zh-Hant", "en-US"]

    let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
    try? handler.perform([request])
    return recognizedLines.joined(separator: "\n")
}

private func renderPageToCGImage(_ page: PDFPage, scale: CGFloat = 1.5) -> CGImage? {
    let mediaBox = page.bounds(for: .mediaBox)
    guard mediaBox.width > 1, mediaBox.height > 1 else { return nil }
    let width = Int((mediaBox.width * scale).clamped(to: 64...2400))
    let height = Int((mediaBox.height * scale).clamped(to: 64...3200))
    let colorSpace = CGColorSpaceCreateDeviceRGB()
    guard let ctx = CGContext(
        data: nil,
        width: width,
        height: height,
        bitsPerComponent: 8,
        bytesPerRow: width * 4,
        space: colorSpace,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    ) else {
        return nil
    }
    ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
    ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
    ctx.saveGState()
    ctx.scaleBy(x: CGFloat(width) / mediaBox.width, y: CGFloat(height) / mediaBox.height)
    ctx.translateBy(x: -mediaBox.origin.x, y: -mediaBox.origin.y)
    page.draw(with: .mediaBox, to: ctx)
    ctx.restoreGState()
    return ctx.makeImage()
}

private extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self {
        min(max(self, range.lowerBound), range.upperBound)
    }
}

private func extractPDF(path: String) -> HelperOutput {
    let url = URL(fileURLWithPath: path)
    guard let doc = PDFDocument(url: url) else {
        return HelperOutput(ok: false, pages: 0, text: nil, error: "failed to open PDF")
    }
    let totalPages = doc.pageCount
    let extractLimit = min(totalPages, 100)
    var sections: [String] = []
    sections.reserveCapacity(extractLimit)

    for i in 0..<extractLimit {
        guard let page = doc.page(at: i), let str = page.string else { continue }
        let trimmed = str.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty {
            sections.append(trimmed)
        }
    }

    var text = sections.joined(separator: "\n")
    // 扫描版纯图像 PDF 无内嵌文本层时，对前几页触发低分辨率 OCR 降级提取。
    if text.count < 24 && totalPages > 0 {
        let ocrLimit = min(totalPages, 6)
        var ocrParts: [String] = []
        for i in 0..<ocrLimit {
            guard let page = doc.page(at: i),
                  let image = renderPageToCGImage(page) else { continue }
            let recognized = recognizeText(in: image)
            if !recognized.isEmpty {
                ocrParts.append(recognized)
            }
        }
        let ocrJoined = ocrParts.joined(separator: "\n")
        if ocrJoined.count > text.count {
            text = ocrJoined
        }
    }

    return HelperOutput(ok: true, pages: UInt32(max(0, totalPages)), text: text, error: nil)
}

private func extractImage(path: String) -> HelperOutput {
    let url = URL(fileURLWithPath: path) as CFURL
    let opts = [kCGImageSourceShouldCache: false] as CFDictionary
    guard let source = CGImageSourceCreateWithURL(url, opts),
          let cgImage = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
        return HelperOutput(ok: false, pages: 0, text: nil, error: "failed to decode image")
    }
    let text = recognizeText(in: cgImage)
    return HelperOutput(ok: true, pages: 1, text: text, error: nil)
}

// 90 秒硬超时熔断：隔离畸形文档解析或识别阻塞，保护宿主进程稳定性。
Darwin.alarm(90)

let args = CommandLine.arguments
guard args.count == 3 else {
    emit(HelperOutput(ok: false, pages: 0, text: nil, error: "usage: LokiiContentHelper --pdf|--image <path>"), exitCode: 64)
}

let mode = args[1]
let targetPath = args[2]
private let result: HelperOutput

switch mode {
case "--pdf":
    result = extractPDF(path: targetPath)
case "--image":
    result = extractImage(path: targetPath)
default:
    emit(HelperOutput(ok: false, pages: 0, text: nil, error: "unknown mode: \(mode)"), exitCode: 64)
}

emit(result, exitCode: result.ok ? 0 : 1)
