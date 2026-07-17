//
//  QRScannerView.swift
//  FlightDeckRemote
//
//  Full-screen QR scanner for the pairing flow (PRD §5.6 "Scan QR
//  instead"). Parses "fdr1:" payloads (see `PairingQRCodec` in
//  PairingModels.swift) and reports them via `onPayload`.
//
//  Degrades gracefully when there's no usable camera:
//   - the simulator has no camera at all — `checkCameraAvailability()`
//     detects this via `AVCaptureDevice.default(for: .video) == nil` and
//     shows "Camera unavailable — enter the code instead" instead of a
//     blank/crashing capture view;
//   - a real device with camera access denied/restricted shows the same
//     fallback with permission-specific guidance.
//  In both fallback cases, DEBUG builds additionally show a text field to
//  paste a raw payload string (`fdr1:…`) so the scan → pair path stays
//  exercisable without a camera (e.g. in the simulator or UI tests).
//

import SwiftUI
import AVFoundation

struct QRScannerView: View {
    var onPayload: (PairingQRPayload) -> Void
    var onEnterCodeInstead: () -> Void
    var onCancel: () -> Void

    @State private var availability: CameraAvailability = .checking
    @State private var torchOn = false
    @State private var scanError: String?
    #if DEBUG
    @State private var debugPayloadText = ""
    #endif

    enum CameraAvailability: Equatable {
        case checking
        case available
        case unavailable(reason: String)
    }

    var body: some View {
        ZStack {
            Theme.bgDeep.ignoresSafeArea()

            switch availability {
            case .checking:
                ProgressView()
                    .tint(Theme.accent)
            case .available:
                cameraContent
            case .unavailable(let reason):
                unavailableContent(reason: reason)
            }

            VStack {
                HStack {
                    Button(action: onCancel) {
                        Image(systemName: "xmark")
                            .font(.system(size: 18, weight: .semibold))
                            .foregroundStyle(Theme.textPrimary)
                            .padding(12)
                            .background(Theme.bgCard, in: Circle())
                    }
                    .accessibilityIdentifier("qr-scanner-cancel-button")
                    .accessibilityLabel("Close scanner")

                    Spacer()

                    if availability == .available {
                        Button {
                            torchOn.toggle()
                        } label: {
                            Image(systemName: torchOn ? "bolt.fill" : "bolt.slash")
                                .font(.system(size: 18, weight: .semibold))
                                .foregroundStyle(Theme.textPrimary)
                                .padding(12)
                                .background(Theme.bgCard, in: Circle())
                        }
                        .accessibilityIdentifier("qr-scanner-torch-button")
                        .accessibilityLabel("Toggle flashlight")
                    }
                }
                .padding(Theme.Spacing.lg)
                Spacer()
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("QRScannerView")
        .onAppear(perform: checkCameraAvailability)
    }

    @ViewBuilder
    private var cameraContent: some View {
        ZStack {
            QRCaptureRepresentable(torchOn: torchOn, onCode: handleScannedString)
                .ignoresSafeArea()

            VStack {
                Spacer()
                VStack(spacing: Theme.Spacing.sm) {
                    if let scanError {
                        Text(scanError)
                            .typography(Typography.callout)
                            .foregroundStyle(Theme.statusRed)
                            .multilineTextAlignment(.center)
                            .accessibilityIdentifier("qr-scanner-error-text")
                    }
                    Text("Point your camera at the QR code shown on your Mac.")
                        .typography(Typography.callout)
                        .foregroundStyle(Theme.textMuted)
                        .multilineTextAlignment(.center)

                    enterCodeInsteadButton
                }
                .padding(Theme.Spacing.xl)
                .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
                .padding(.horizontal, Theme.Spacing.xl)
                .padding(.bottom, Theme.Spacing.xxxl)
            }
        }
    }

    @ViewBuilder
    private func unavailableContent(reason: String) -> some View {
        VStack(spacing: Theme.Spacing.lg) {
            Image(systemName: "camera.metering.unknown")
                .font(.system(size: 40))
                .foregroundStyle(Theme.textMutedDark)
            Text(reason)
                .typography(Typography.body)
                .foregroundStyle(Theme.textMuted)
                .multilineTextAlignment(.center)
                .padding(.horizontal, Theme.Spacing.xxl)
                .accessibilityIdentifier("qr-scanner-unavailable-text")

            enterCodeInsteadButton

            #if DEBUG
            VStack(spacing: Theme.Spacing.sm) {
                Text("DEBUG: paste a payload string to simulate a scan")
                    .typography(Typography.caption)
                    .foregroundStyle(Theme.textMutedDark)
                TextField("fdr1:…", text: $debugPayloadText)
                    .typography(Typography.mono)
                    .foregroundStyle(Theme.textPrimary)
                    .padding(Theme.Spacing.md)
                    .background(Theme.bgField, in: RoundedRectangle(cornerRadius: Theme.Radius.card, style: .continuous))
                    .autocorrectionDisabled()
                    .textInputAutocapitalization(.never)
                    .accessibilityIdentifier("qr-scanner-debug-payload-field")

                Button("Use payload") {
                    handleScannedString(debugPayloadText)
                }
                .typography(Typography.bodyMedium)
                .foregroundStyle(Theme.accent)
                .accessibilityIdentifier("qr-scanner-debug-use-payload-button")
            }
            .padding(.horizontal, Theme.Spacing.xxl)
            .padding(.top, Theme.Spacing.lg)
            #endif
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var enterCodeInsteadButton: some View {
        Button("Enter code instead", action: onEnterCodeInstead)
            .typography(Typography.bodyMedium)
            .foregroundStyle(Theme.accent)
            .accessibilityIdentifier("qr-scanner-enter-code-instead-button")
    }

    private func checkCameraAvailability() {
        // No camera hardware at all (every simulator, some restricted
        // devices) — never attempt to start a capture session.
        guard AVCaptureDevice.default(for: .video) != nil else {
            availability = .unavailable(reason: "Camera unavailable — enter the code instead.")
            return
        }

        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            availability = .available
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { granted in
                Task { @MainActor in
                    availability = granted
                        ? .available
                        : .unavailable(reason: "Camera access is off — enable it in Settings, or enter the code instead.")
                }
            }
        case .denied, .restricted:
            availability = .unavailable(reason: "Camera access is off — enable it in Settings, or enter the code instead.")
        @unknown default:
            availability = .unavailable(reason: "Camera unavailable — enter the code instead.")
        }
    }

    private func handleScannedString(_ raw: String) {
        do {
            let payload = try PairingQRCodec.decode(raw)
            scanError = nil
            onPayload(payload)
        } catch {
            scanError = "That QR code isn't a FlightDeck pairing code."
        }
    }
}

// MARK: - AVFoundation plumbing

/// `UIViewControllerRepresentable` wrapper around an `AVCaptureSession`
/// configured for QR-only metadata detection. Kept separate from
/// `QRScannerView` so the SwiftUI-facing surface stays a plain `View` and
/// all AVFoundation lifecycle/session plumbing lives in one place.
private struct QRCaptureRepresentable: UIViewControllerRepresentable {
    var torchOn: Bool
    var onCode: (String) -> Void

    func makeUIViewController(context: Context) -> QRCaptureViewController {
        let controller = QRCaptureViewController()
        controller.onCode = onCode
        return controller
    }

    func updateUIViewController(_ uiViewController: QRCaptureViewController, context: Context) {
        uiViewController.setTorch(on: torchOn)
    }
}

/// Owns the `AVCaptureSession` + preview layer + metadata output. Reports
/// each detected QR string via `onCode`, de-duplicating consecutive
/// detections of the same string (a QR sits in frame across many capture
/// callbacks) — the caller (`QRScannerView`) handles the resulting payload
/// once and dismisses.
final class QRCaptureViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onCode: ((String) -> Void)?

    private let session = AVCaptureSession()
    private var previewLayer: AVCaptureVideoPreviewLayer?
    private var lastHandledString: String?

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        configureSession()
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer?.frame = view.bounds
    }

    private func configureSession() {
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device),
              session.canAddInput(input) else { return }
        session.addInput(input)

        let output = AVCaptureMetadataOutput()
        guard session.canAddOutput(output) else { return }
        session.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: .main)
        output.metadataObjectTypes = [.qr]

        let layer = AVCaptureVideoPreviewLayer(session: session)
        layer.videoGravity = .resizeAspectFill
        layer.frame = view.bounds
        view.layer.addSublayer(layer)
        previewLayer = layer

        DispatchQueue.global(qos: .userInitiated).async { [session] in
            session.startRunning()
        }
    }

    func setTorch(on: Bool) {
        guard let device = AVCaptureDevice.default(for: .video), device.hasTorch else { return }
        try? device.lockForConfiguration()
        device.torchMode = on ? .on : .off
        device.unlockForConfiguration()
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard let object = metadataObjects.first as? AVMetadataMachineReadableCodeObject,
              object.type == .qr,
              let string = object.stringValue,
              string != lastHandledString else { return }
        lastHandledString = string
        onCode?(string)
    }

    deinit {
        session.stopRunning()
    }
}
