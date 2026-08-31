import SwiftUI

@main
struct SynthHiresBridgeApp: App {
    @StateObject private var store: BridgeStore
    @StateObject private var bridge: BridgeService

    init() {
        let store = BridgeStore()
        _store = StateObject(wrappedValue: store)
        _bridge = StateObject(wrappedValue: BridgeService(store: store))
    }

    var body: some Scene {
        WindowGroup {
            ContentView(store: store, bridge: bridge)
                .task {
                    if store.deviceID != nil { bridge.start() }
                }
        }
    }
}
