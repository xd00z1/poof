import SwiftUI

struct FavoritesView: View {
    @EnvironmentObject private var store: AppStore
    @Environment(\.dismiss) private var dismiss
    let onPick: (Favorite) -> Void

    var body: some View {
        NavigationStack {
            List {
                if store.favorites.isEmpty {
                    Text("No favorites yet. Pick a spot and tap ☆+.").foregroundStyle(.secondary)
                }
                ForEach(store.favorites) { fav in
                    Button {
                        onPick(fav)
                        dismiss()
                    } label: {
                        VStack(alignment: .leading) {
                            Text(fav.name)
                            Text(String(format: "%.5f, %.5f", fav.lat, fav.lon))
                                .font(.caption.monospaced())
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                .onDelete { store.favorites.remove(atOffsets: $0) }
                .onMove { store.favorites.move(fromOffsets: $0, toOffset: $1) }
            }
            .navigationTitle("Favorites")
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { EditButton() }
                ToolbarItem(placement: .topBarTrailing) { Button("Done") { dismiss() } }
            }
        }
    }
}
