# SynthHires Bridge for iOS

Native SwiftUI client for iOS 16+.

## Build

```bash
brew install xcodegen
xcodegen generate
xcodebuild -project SynthHiresBridge.xcodeproj -scheme SynthHiresBridge -sdk iphonesimulator -configuration Debug CODE_SIGNING_ALLOWED=NO build
```

The CI workflow performs the same unsigned simulator build. Device/TestFlight distribution requires an Apple Team, signing certificate and provisioning profile supplied as repository secrets.

## Real iOS capabilities

- `mobile.location.read`: Core Location, foreground permission.
- `mobile.contacts.read`: Contacts permission.
- `mobile.clipboard.read` / `mobile.clipboard.write`: UIPasteboard, with consent for writes.

The following are intentionally reported as unsupported on iOS rather than shown as working: reading/dismissing arbitrary third-party notifications, reading SMS, headless SMS sending and cross-app accessibility automation. Apple does not expose those APIs to a general background bridge.
