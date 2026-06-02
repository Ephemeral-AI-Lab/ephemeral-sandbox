mod composer;
mod engine;
mod scope;
mod section;
mod xml;

pub use composer::{render_task_guidance, AgentEntryComposer, AgentEntryMessages};
pub use engine::{ContextEngine, ContextEngineDeps};
pub use scope::ContextScope;
pub use section::{AgentContext, ContextRole, ContextSection};
pub use xml::render_context_xml;
