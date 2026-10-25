//
//  ChatImageAttachment.swift
//  FlightDeckRemote
//
//  Images attached to a reply (remote-control-ty3p). A picked photo is
//  downscaled and re-encoded as JPEG here, so what crosses the link is a few
//  hundred KB rather than a 12 MP original, then sent as
//  `attach_image_chunk` commands of at most `Wire.maxImageChunkBytes` (one
//  envelope stays inside the direct link's message limit). The reply that
//  follows names the uploads; the desktop saves each image and types its path
//  ahead of the text, as a pasted image is on the desktop.
//
//  Offered only when the desktop's snapshot lists `Wire.featureImageAttach`:
//  an older desktop drops commands it cannot parse.
//

import Foundation
import UIKit

/// One image waiting to be sent with the next reply (or kept with a sent
/// message so a failed send can be retried).
struct ImageAttachment: Identifiable, Hashable, Sendable {
    /// The upload id the chunks and the reply share.
    let id: String
    /// The encoded image.
    let data: Data
    /// Its format, as the file extension the desktop saves it under.
    let format: String

    /// The image's chunks, each base64 of at most `chunkBytes` bytes, in order.
    func chunks(of chunkBytes: Int = Wire.maxImageChunkBytes) -> [String] {
        stride(from: 0, to: data.count, by: chunkBytes).map { start in
            data.subdata(in: start..<min(start + chunkBytes, data.count)).base64EncodedString()
        }
    }

    /// The commands that upload this image to `sessionId`, in send order.
    func uploadCommands(for sessionId: Wire.SessionId) -> [Wire.CommandBody] {
        let parts = chunks()
        return parts.enumerated().map { index, part in
            .attachImageChunk(sessionId: sessionId, uploadId: id, format: format,
                              index: index, count: parts.count, data: part)
        }
    }
}

enum ImageAttachmentEncoder {
    /// The longest side, in pixels, an attached photo is scaled down to —
    /// plenty for an agent to read a screenshot, a fraction of a camera photo.
    static let maxDimension: CGFloat = 2048
    /// JPEG quality for the re-encode.
    static let jpegQuality: CGFloat = 0.8
    /// The largest image the desktop saves (`MAX_PASTED_IMAGE_BYTES`).
    static let maxBytes = 10 * 1024 * 1024
    /// Most images attached to one reply.
    static let maxPerReply = 4

    /// `data` (any format UIKit decodes: HEIC, PNG, JPEG…) as a JPEG attachment
    /// no larger than `maxDimension` on its longest side, or `nil` when it is
    /// not an image or still too large.
    static func attachment(from data: Data, id: String = newUploadId()) -> ImageAttachment? {
        guard let image = UIImage(data: data) else { return nil }
        let longest = max(image.size.width, image.size.height)
        let scale = longest > maxDimension ? maxDimension / longest : 1
        let size = CGSize(width: (image.size.width * scale).rounded(),
                          height: (image.size.height * scale).rounded())
        let format = UIGraphicsImageRendererFormat.default()
        format.scale = 1
        let scaled = UIGraphicsImageRenderer(size: size, format: format).image { _ in
            image.draw(in: CGRect(origin: .zero, size: size))
        }
        guard let jpeg = scaled.jpegData(compressionQuality: jpegQuality),
              !jpeg.isEmpty, jpeg.count <= maxBytes else { return nil }
        return ImageAttachment(id: id, data: jpeg, format: "jpg")
    }

    static func newUploadId() -> String {
        "up_" + UUID().uuidString.replacingOccurrences(of: "-", with: "").prefix(16).lowercased()
    }
}
