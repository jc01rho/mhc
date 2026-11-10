//! Native port of the OmOMeow gateway's worker extension (omo-nyang `src/model-provider.ts` with the
//! `session-move-extension.ts` and `fleet-tool.ts` it registers). The gateway passes the TS entry as
//! `--extension`; the CLI maps that path to this crate by package name.

pub mod fleet_tool;
pub mod model_provider;
pub mod session_move_extension;

use maho_ext_api::{Extension, ExtensionApi};
use std::sync::Arc;

pub struct OmOMeow;

impl Extension for OmOMeow {
    fn register(&self, api: &mut ExtensionApi) {
        let result = session_move_extension::register_move_readiness(api)
            .and_then(|()| {
                let env = Arc::new(session_move_extension::process_environment());
                session_move_extension::register_session_move_extension(api, env);
                let source = api.registered.source_info.path.clone();
                match fleet_tool::script_directory(&source) {
                    Some(directory) => fleet_tool::register_fleet_tool(
                        api,
                        fleet_tool::bun_call(fleet_tool::fleet_tool_script(&directory)),
                    ),
                    None => Ok(()),
                }
            })
            .and_then(|()| model_provider::register(api));
        if let Err(error) = result {
            std::panic::panic_any(error);
        }
    }
}
