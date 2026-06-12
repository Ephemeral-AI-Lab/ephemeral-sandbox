use tracing::Subscriber;
use tracing_subscriber::layer::SubscriberExt;

use crate::layer::TraceSpoolLayer;

pub fn registry_with_trace_layer(
    layer: TraceSpoolLayer,
) -> impl Subscriber + Send + Sync + 'static {
    tracing_subscriber::registry().with(layer)
}
