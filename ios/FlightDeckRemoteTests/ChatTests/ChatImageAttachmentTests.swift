//
//  ChatImageAttachmentTests.swift
//  FlightDeckRemoteTests
//
//  Images attached to a reply (remote-control-ty3p): chunking, the send order
//  (chunks, then the reply naming them), the desktop-feature gate, retry, and
//  reconciling the optimistic row against the desktop's echo.
//

import Testing
import Foundation
import UIKit
@testable import FlightDeckRemote

@MainActor
@Suite struct ChatImageAttachmentTests {

    private func makeModel(imageAttach: Bool = true) -> (ChatViewModel, FakeChatSender) {
        let sender = FakeChatSender()
        let gate = CommandsPausedGate(source: FakeConnectionSource(.connected(latencyMs: 5)),
                                      launchArguments: [])
        let model = ChatViewModel(projectId: Wire.ProjectId("p1"),
                                  sessionId: Wire.SessionId("s1"), now: { 1_000 })
        model.configureSend(sender: sender, pausedGate: gate)
        model.imageAttachOverride = imageAttach
        return (model, sender)
    }

    private func image(_ id: String, bytes: Int) -> ImageAttachment {
        ImageAttachment(id: id, data: Data((0..<bytes).map { UInt8($0 % 251) }), format: "jpg")
    }

    @Test func chunksSplitAtTheLimitAndReassembleExactly() {
        let attachment = image("up_1", bytes: Wire.maxImageChunkBytes * 2 + 10)
        let chunks = attachment.chunks()
        #expect(chunks.count == 3)
        let joined = chunks.compactMap { Data(base64Encoded: $0) }.reduce(Data(), +)
        #expect(joined == attachment.data)
        #expect(chunks.dropLast().allSatisfy { Data(base64Encoded: $0)?.count == Wire.maxImageChunkBytes })
    }

    @Test func sendUploadsEveryChunkThenTheReplyNamingTheImages() {
        let (model, sender) = makeModel()
        model.attach(image("up_a", bytes: Wire.maxImageChunkBytes + 1))
        model.attach(image("up_b", bytes: 3))
        model.draft = "what is wrong here?"
        model.send()

        let bodies = sender.sends.map(\.body)
        #expect(bodies.count == 4)
        guard case let .attachImageChunk(_, upload0, format, index0, count0, _) = bodies[0],
              case let .attachImageChunk(_, upload1, _, index1, count1, _) = bodies[1],
              case let .attachImageChunk(_, upload2, _, index2, count2, _) = bodies[2],
              case let .reply(sessionId, text, imageIds) = bodies[3] else {
            Issue.record("expected three chunks then a reply: \(bodies)")
            return
        }
        #expect((upload0, index0, count0, format) == ("up_a", 0, 2, "jpg"))
        #expect((upload1, index1, count1) == ("up_a", 1, 2))
        #expect((upload2, index2, count2) == ("up_b", 0, 1))
        #expect(sessionId == Wire.SessionId("s1"))
        #expect(text == "what is wrong here?")
        #expect(imageIds == ["up_a", "up_b"])

        // The draft and attachments clear; the optimistic row marks the images.
        #expect(model.attachments.isEmpty)
        #expect(model.draft.isEmpty)
        #expect(model.outgoing.first?.displayText == "[Image] [Image] what is wrong here?")
        // Only the reply is tracked: its outcome stands for the message.
        #expect(model.outgoing.first?.commandId == sender.lastCommandId)
    }

    @Test func anImageAloneCanBeSent() {
        let (model, sender) = makeModel()
        model.attach(image("up_a", bytes: 3))
        model.send()
        guard case let .reply(_, text, imageIds) = sender.sends.last?.body else {
            Issue.record("expected a reply")
            return
        }
        #expect(text.isEmpty)
        #expect(imageIds == ["up_a"])
    }

    @Test func aDesktopWithoutTheFeatureIsNeverSentImages() {
        let (model, sender) = makeModel(imageAttach: false)
        #expect(model.canAttachImages == false)
        model.attach(image("up_a", bytes: 3))
        #expect(model.attachments.isEmpty)
        model.draft = "plain"
        model.send()
        #expect(sender.sends.count == 1)
        if case let .reply(_, _, imageIds) = sender.sends[0].body {
            #expect(imageIds.isEmpty)
        }
    }

    @Test func attachmentsAreCappedAndRemovable() {
        let (model, _) = makeModel()
        for i in 0..<(ImageAttachmentEncoder.maxPerReply + 1) {
            model.attach(image("up_\(i)", bytes: 3))
        }
        #expect(model.attachments.count == ImageAttachmentEncoder.maxPerReply)
        #expect(model.canAttachMoreImages == false)
        model.removeAttachment("up_0")
        #expect(model.attachments.map(\.id).contains("up_0") == false)
        #expect(model.canAttachMoreImages == true)
    }

    @Test func retryReuploadsTheImagesBeforeTheReply() {
        let (model, sender) = makeModel()
        model.attach(image("up_a", bytes: 3))
        model.draft = "see"
        model.send()
        model.applyDelivery(commandId: sender.lastCommandId!, state: .delivered(.rejected))
        let localId = model.outgoing[0].localId
        model.retryOutgoing(localId)

        let bodies = sender.sends.map(\.body).suffix(2)
        guard case .attachImageChunk = bodies.first, case let .reply(_, _, ids) = bodies.last else {
            Issue.record("expected the chunk to be resent before the reply")
            return
        }
        #expect(ids == ["up_a"])
    }

    @Test func anImageReplyReconcilesAgainstAnEchoEndingWithItsText() {
        let msg = OutgoingMessage(localId: Wire.ItemId("local-1"), text: "see this",
                                  issuedAtMs: 1_000, commandId: Wire.CommandId("c"),
                                  state: .sent, images: [image("up_a", bytes: 3)])
        let echo = { (text: String, at: Int64) in
            [Wire.TranscriptItem.userMessage(itemId: Wire.ItemId("u"), text: text, atMs: at)]
        }
        #expect(ChatSendLogic.isReconciled(msg, against: echo("[Image #1] see this", 1_200)))
        #expect(ChatSendLogic.isReconciled(msg, against: echo("/tmp/p/1.jpg see this", 1_200)))
        #expect(!ChatSendLogic.isReconciled(msg, against: echo("see this", 1_200)),
                "the bare text is a different, text-only message")

        var imageOnly = msg
        imageOnly.text = ""
        #expect(ChatSendLogic.isReconciled(imageOnly, against: echo("[Image #1]", 1_200)))
        #expect(!ChatSendLogic.isReconciled(imageOnly, against: echo("[Image #1]", 900)),
                "an earlier image message is not this one")
    }

    @Test func encoderDownscalesToAJpegWithinTheLimit() throws {
        let big = UIGraphicsImageRenderer(size: CGSize(width: 4000, height: 1000),
                                          format: {
                                              let f = UIGraphicsImageRendererFormat.default()
                                              f.scale = 1
                                              return f
                                          }())
            .image { ctx in
                UIColor.orange.setFill()
                ctx.fill(CGRect(x: 0, y: 0, width: 4000, height: 1000))
            }
        let png = try #require(big.pngData())
        let attachment = try #require(ImageAttachmentEncoder.attachment(from: png, id: "up_x"))
        #expect(attachment.format == "jpg")
        #expect(attachment.id == "up_x")
        let decoded = try #require(UIImage(data: attachment.data))
        #expect(max(decoded.size.width, decoded.size.height) <= ImageAttachmentEncoder.maxDimension)
        #expect(ImageAttachmentEncoder.attachment(from: Data("not an image".utf8)) == nil)
    }
}
