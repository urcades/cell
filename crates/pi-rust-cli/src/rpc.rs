use std::io::{Read, Write};

use pi_rust_core::AgentSession;

pub(crate) fn run_rpc_with_io(
    reader: impl Read + Send + 'static,
    writer: impl Write,
    session: AgentSession,
) -> Result<i32, String> {
    pi_rust_rpc::run_rpc_with_io(reader, writer, session)
}
