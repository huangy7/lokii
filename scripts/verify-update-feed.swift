import Foundation
import CryptoKit

enum ValidationError: Error {
    case invalid(String)
}

func require(_ condition: Bool, _ message: String) throws {
    if !condition { throw ValidationError.invalid(message) }
}

func validateFeed() throws {
    let arguments = CommandLine.arguments
    try require(arguments.count == 6, "Usage: verify-update-feed <feed> <archive> <public-key> <version> <architecture>")
    let document = try XMLDocument(contentsOf: URL(fileURLWithPath: arguments[1]), options: [.nodeLoadExternalEntitiesNever])
    let items = try document.nodes(forXPath: "/rss/channel/item")
    try require(!items.isEmpty, "Empty update feed")
    for candidate in items {
        if let candidateVersion = try candidate.nodes(forXPath: "sparkle:version").first?.stringValue {
            try require(candidateVersion.compare(arguments[4], options: .numeric) != .orderedDescending, "Refusing to publish an older release")
        }
    }
    let archiveURL = URL(fileURLWithPath: arguments[2])
    let expectedURL = "https://github.com/huangy7/lokii/releases/download/v\(arguments[4])/\(archiveURL.lastPathComponent)"
    let matching = try items.filter { item in
        let enclosures = try item.nodes(forXPath: "enclosure")
        return (enclosures.first as? XMLElement)?.attribute(forName: "url")?.stringValue == expectedURL
    }
    try require(matching.count == 1, "Missing or duplicate current update")
    guard let item = matching.first,
          let enclosure = try item.nodes(forXPath: "enclosure").first as? XMLElement,
          let encodedSignature = enclosure.attribute(forName: "sparkle:edSignature")?.stringValue,
          let signature = Data(base64Encoded: encodedSignature),
          let publicData = Data(base64Encoded: arguments[3]) else {
        throw ValidationError.invalid("Missing update signature")
    }
    let archive = try Data(contentsOf: archiveURL)
    try require(enclosure.attribute(forName: "length")?.stringValue == String(archive.count), "Incorrect archive length")
    let key = try Curve25519.Signing.PublicKey(rawRepresentation: publicData)
    try require(key.isValidSignature(signature, for: archive), "Invalid archive signature")
    try require(archiveURL.lastPathComponent == "Lokii-\(arguments[4])-\(arguments[5]).zip", "Incorrect architecture filename")
    try require(try item.nodes(forXPath: "sparkle:shortVersionString").first?.stringValue == arguments[4], "Incorrect version")
    try require(try item.nodes(forXPath: "sparkle:version").first?.stringValue == arguments[4], "Incorrect build version")
    try require(try item.nodes(forXPath: "sparkle:minimumSystemVersion").first?.stringValue == "13.0", "Incorrect system requirement")
    try require(try document.nodes(forXPath: "//sparkle:deltas").isEmpty, "Delta updates are disabled")
    print("更新清单和更新包签名检查通过：\(arguments[5])")
}

do {
    try validateFeed()
} catch {
    FileHandle.standardError.write(Data("更新清单检查失败：\(error)\n".utf8))
    exit(1)
}
