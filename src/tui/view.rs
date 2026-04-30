use crate::tui::App;

/// Read-only projection of `App` for rendering.
///
/// Ensures render functions cannot mutate application state.
/// Created via `App::view()` and provides transparent access
/// to all `App` fields through `Deref`.
pub struct AppView<'a>(pub(crate) &'a App);

impl<'a> std::ops::Deref for AppView<'a> {
    type Target = App;
    fn deref(&self) -> &App {
        self.0
    }
}

impl<'a> AppView<'a> {
    /// Whether a search request is currently in flight.
    /// Renderers should call this instead of inspecting internal
    /// state — `App.search.current` is `pub(crate)`.
    pub fn is_searching(&self) -> bool {
        self.0.is_searching()
    }
}
