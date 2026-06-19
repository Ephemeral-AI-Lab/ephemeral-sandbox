use std::collections::HashMap;
use std::time::Instant;

use crate::isolated_network_setup::IsolatedNetwork;
use crate::model::NetworkMode;
use crate::namespace::{NamespacePlan, NamespaceRuntime};
use crate::profile::common::{
    record_phase_ms, ProfileHooks, WorkspaceProfileNetworkContext,
    WorkspaceProfileNetworkTeardownContext,
};
use crate::profile::manager::IsolatedNetworkError;

pub(crate) struct IsolatedProfile<'a> {
    network: &'a mut IsolatedNetwork,
    fallback_dns: &'a str,
    setup_timeout_s: f64,
}

impl<'a> IsolatedProfile<'a> {
    pub(crate) fn new(
        network: &'a mut IsolatedNetwork,
        fallback_dns: &'a str,
        setup_timeout_s: f64,
    ) -> Self {
        Self {
            network,
            fallback_dns,
            setup_timeout_s,
        }
    }
}

impl ProfileHooks for IsolatedProfile<'_> {
    fn kind(&self) -> NetworkMode {
        NetworkMode::Isolated
    }

    fn namespace_plan(&self) -> NamespacePlan {
        NamespacePlan::isolated_network()
    }

    fn setup_network_after_namespace(
        &mut self,
        runtime: &NamespaceRuntime,
        context: &mut WorkspaceProfileNetworkContext<'_>,
        phases_ms: &mut HashMap<String, f64>,
    ) -> Result<(), IsolatedNetworkError> {
        let phase_start = Instant::now();
        let veth = if runtime.stub {
            self.network.install_stub_veth(&context.workspace_id().0)?
        } else {
            self.network.initialize()?;
            self.network
                .install_veth(&context.workspace_id().0, context.holder_pid())?
        };
        context.set_veth(veth);
        record_phase_ms(phases_ms, "install_veth", phase_start);
        Ok(())
    }

    fn setup_network_after_mount(
        &mut self,
        runtime: &NamespaceRuntime,
        context: &mut WorkspaceProfileNetworkContext<'_>,
        phases_ms: &mut HashMap<String, f64>,
    ) -> Result<(), IsolatedNetworkError> {
        let phase_start = Instant::now();
        context.configure_dns(runtime, self.fallback_dns, self.setup_timeout_s)?;
        record_phase_ms(phases_ms, "configure_dns", phase_start);
        context.signal_net_ready(runtime, self.setup_timeout_s)
    }

    fn teardown_network(
        &mut self,
        runtime: &NamespaceRuntime,
        context: &WorkspaceProfileNetworkTeardownContext<'_>,
        phases_ms: &mut HashMap<String, f64>,
    ) {
        let phase_start = Instant::now();
        if let Some(veth) = context.veth() {
            if runtime.stub {
                self.network.release_stub_veth(veth);
            } else {
                self.network.teardown_veth(veth);
            }
        }
        record_phase_ms(phases_ms, "teardown_veth", phase_start);
    }
}
