import SwiftUI

struct ContentView: View {
    @ObservedObject var store: BridgeStore
    @ObservedObject var bridge: BridgeService
    @State private var code = ""
    @State private var pairing = false
    @State private var message = "Introduce el código mostrado en SynthHires."

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("SynthHires Bridge").font(.system(size: 30, weight: .bold, design: .rounded))
                        Text("Conexión local-first, permisos explícitos y control total.").foregroundStyle(.secondary)
                    }
                    statusCard
                    VStack(alignment: .leading, spacing: 10) {
                        Text("Emparejar dispositivo").font(.headline)
                        TextField("https://synthhires.com", text: $store.backendURL).textInputAutocapitalization(.never).textFieldStyle(.roundedBorder).keyboardType(.URL)
                        TextField("Código de 6 caracteres", text: $code).textInputAutocapitalization(.characters).textFieldStyle(.roundedBorder)
                        Button(pairing ? "Emparejando…" : "Emparejar y activar") { pair() }.buttonStyle(.borderedProminent).disabled(pairing || code.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        Text(message).font(.footnote).foregroundStyle(.secondary)
                    }.padding(18).background(.thinMaterial, in: RoundedRectangle(cornerRadius: 20))
                    Text("iOS solo muestra capabilities que Apple permite ejecutar de forma honesta. SMS, notificaciones de terceros y automatización de otras apps no están disponibles.").font(.footnote).foregroundStyle(.secondary)
                }.padding(20)
            }.navigationTitle("Bridge").alert("Permiso requerido", isPresented: Binding(get: { bridge.pendingConsent != nil }, set: { if !$0 { bridge.approveConsent(false) } })) {
                Button("Denegar", role: .cancel) { bridge.approveConsent(false) }
                Button("Permitir") { bridge.approveConsent(true) }
            } message: { Text(bridge.pendingConsent?.capability ?? "") }
        }
    }

    private var statusCard: some View {
        HStack(spacing: 12) {
            Circle().fill(bridge.connected ? .green : .orange).frame(width: 12, height: 12)
            VStack(alignment: .leading) { Text(bridge.connected ? "Conectado" : "Desconectado").font(.headline); Text(bridge.lastError ?? (store.deviceID == nil ? "Sin emparejar" : "Listo para reconectar")).font(.footnote).foregroundStyle(.secondary) }
            Spacer()
            if store.deviceID != nil { Button("Iniciar") { bridge.start() }.buttonStyle(.bordered) }
        }.padding(18).background(.thinMaterial, in: RoundedRectangle(cornerRadius: 20))
    }

    private func pair() {
        pairing = true; message = "Validando código…"
        guard let url = URL(string: store.backendURL.trimmingCharacters(in: CharacterSet(charactersIn: "/")) + "/api/devices/pair/complete") else { pairing = false; message = "Backend inválido"; return }
        var request = URLRequest(url: url); request.httpMethod = "POST"; request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: ["code": code.uppercased(), "deviceKind": "mobile", "deviceName": UIDevice.current.name, "fingerprint": UIDevice.current.identifierForVendor?.uuidString ?? UUID().uuidString, "desiredScopes": ["mobile.contacts.read", "mobile.location.read", "mobile.clipboard.read", "mobile.clipboard.write"]])
        URLSession.shared.dataTask(with: request) { data, response, error in
            DispatchQueue.main.async {
                pairing = false
                guard error == nil, let data, let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode), let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any], let payload = json["data"] as? [String: Any], let id = payload["deviceId"] as? String, let token = payload["token"] as? String, let scope = payload["scopes"] as? [String: Any] else { message = error?.localizedDescription ?? "Código inválido o expirado"; return }
                let scopes = BridgeScopes(capabilities: scope["capabilities"] as? [String] ?? [], alwaysAllowPaths: scope["alwaysAllowPaths"] as? [String] ?? [])
                store.savePair(deviceID: id, token: token, backendURL: store.backendURL, scopes: scopes); message = "Emparejado correctamente"; bridge.start()
            }
        }.resume()
    }
}
