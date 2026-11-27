//! The namespaces of the omo SDK root (first-party design 4.2), in the order the design lists them.

pub const NAMESPACES: [&str; 14] = [
    "threads",
    "devices",
    "account",
    "deviceService",
    "sync",
    "config",
    "daemon",
    "diagnostics",
    "install",
    "engine",
    "mcp",
    "boulder",
    "worktrees",
    "toolkit",
];

pub fn is_namespace(name: &str) -> bool {
    NAMESPACES.contains(&name)
}
