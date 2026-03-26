use std::io::{Read, Write};

use cell_core::AgentSession;

pub(crate) fn run_rpc_with_io(
    reader: impl Read + Send + 'static,
    writer: impl Write,
    session: AgentSession,
) -> Result<i32, String> {
    cell_rpc::run_rpc_with_io(reader, writer, session)
}
