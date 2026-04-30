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
