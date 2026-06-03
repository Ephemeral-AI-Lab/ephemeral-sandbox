use eos_state::ExecutionTaskOutcome;

use super::{AgentContext, ContextSection};

/// Render a context packet into the XML-like prompt envelope.
#[must_use]
pub fn render_context_xml(context: &AgentContext) -> String {
    let root = ContextSection::new("context")
        .with_attrs(vec![("role".to_owned(), context.role.as_str().to_owned())])
        .with_children(context.sections.clone());
    format!("{}\n", render_section(&root))
}

pub(crate) fn render_task_outcome(outcome: &ExecutionTaskOutcome) -> ContextSection {
    ContextSection::new("task")
        .with_attrs(vec![
            ("task_id".to_owned(), outcome.task_id.as_str().to_owned()),
            (
                "role".to_owned(),
                format!("{:?}", outcome.role).to_lowercase(),
            ),
            (
                "status".to_owned(),
                format!("{:?}", outcome.status).to_lowercase(),
            ),
        ])
        .with_text(outcome.outcome.clone())
}

pub(crate) fn render_section(section: &ContextSection) -> String {
    let attrs = section
        .attrs
        .iter()
        .map(|(k, v)| format!(" {}=\"{}\"", escape(k), escape(v)))
        .collect::<String>();
    let mut body = Vec::new();
    if let Some(text) = &section.text {
        body.push(escape(text));
    }
    body.extend(section.children.iter().map(render_section));
    format!(
        "<{}{}>\n{}\n</{}>",
        section.tag,
        attrs,
        body.join("\n"),
        section.tag
    )
}

fn escape(s: &str) -> String {
    // Matches Python `html.escape(s, quote=True)` (xml.py): `&` first, then the
    // angle brackets, then both quote forms (`"` -> `&quot;`, `'` -> `&#x27;`).
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}
