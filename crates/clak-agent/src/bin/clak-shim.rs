//! `clak-shim`: the MCP server over stdio for clients without a shell. Stdout is the MCP
//! channel: everything that is not protocol goes to stderr.

fn main() {
    clak_agent::shim::main();
}
