pub mod group;
pub mod message;
pub mod ripgrep;

pub use group::{SessionGroup, count_session_messages, group_by_session};
pub use message::Message;
pub(crate) use ripgrep::CANCELLED_ERR;
pub use ripgrep::{
    RipgrepMatch, SearchResult, extract_context, extract_context_around_span,
    extract_project_from_path, sanitize_content, search_multiple_paths,
};
