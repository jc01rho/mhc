//! The transaction helpers one extension op may call.
//!
//! Upstream counterpart: `thread/gateway/extension-transaction.ts`. An op runs its statements and one read-only
//! lookup of the removed core bindings (`binding_for`) inside its own transaction; every helper is refused once the
//! transaction ended.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use super::store_extensions::{
    LegacyBinding, LegacyBindingWhere, StoreExtensionError, StoreExtensionTransaction,
};
use super::store_ops::{SqlAccess, SqlRow, StoreContext, StoreError, rfc3339, row_str, v_str};

/// The transaction ended (the op returned, or its budget ran out).
pub fn ended_error(extension: &str) -> StoreExtensionError {
    StoreExtensionError::failed(format!("The {extension} extension transaction has ended."))
}

/// Rejects a statement that carries more than one statement, so an extension cannot smuggle a second
/// one past the schema checks.
pub fn single_extension_statement(statement: &str) -> Result<(), StoreExtensionError> {
    let mut in_single = false;
    let mut in_double = false;
    let mut terminators = 0;
    for character in statement.chars() {
        match character {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ';' if !in_single && !in_double => terminators += 1,
            _ => {}
        }
    }
    let trimmed = statement.trim_end();
    let trailing = trimmed.ends_with(';');
    if terminators > usize::from(trailing) {
        return Err(StoreExtensionError {
            code: super::store_extensions::StoreExtensionRefusalCode::ExtensionSchemaViolation,
            message: "an extension statement must be exactly one SQL statement.".to_owned(),
        });
    }
    Ok(())
}

struct Guard {
    active: AtomicBool,
}

/// The thread's active, unexpired binding in the `bindings` table the store still keeps (`schema.rs`), or none: the
/// table is absent from a store that never had one.
fn legacy_binding_for(
    ctx: &StoreContext,
    lookup: &LegacyBindingWhere,
    now: i64,
) -> Result<Option<LegacyBinding>, StoreError> {
    if ctx
        .one(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'bindings'",
            &[],
        )?
        .is_none()
    {
        return Ok(None);
    }
    let row = ctx.one(
        "SELECT binding_id, policy_id, session_durable_id FROM bindings WHERE platform = ? AND account_id = ? AND chat_id = ? AND thread_id = ? AND status = 'active' AND (expires_at IS NULL OR expires_at > ?)",
        &[
            v_str(&lookup.platform),
            v_str(&lookup.account_id),
            v_str(&lookup.chat_id),
            v_str(&lookup.thread_id),
            v_str(rfc3339(now)),
        ],
    )?;
    Ok(row.map(|row| LegacyBinding {
        binding_id: row_str(&row, "binding_id"),
        policy_id: row_str(&row, "policy_id"),
        session_durable_id: row_str(&row, "session_durable_id"),
    }))
}

/// Builds the helpers of one op's transaction; `finish` marks the transaction ended.
pub struct ExtensionTransaction<'a> {
    pub tx: StoreExtensionTransaction<'a>,
    guard: Arc<Guard>,
}

impl ExtensionTransaction<'_> {
    pub fn finish(&self) {
        self.guard.active.store(false, Ordering::SeqCst);
    }
}

pub fn extension_transaction<'a>(
    ctx: &'a StoreContext,
    name: &str,
    now: i64,
) -> ExtensionTransaction<'a> {
    let guard = Arc::new(Guard {
        active: AtomicBool::new(true),
    });
    let owner = name.to_owned();

    let sql_guard = {
        let guard = guard.clone();
        let owner = owner.clone();
        move || -> Result<(), StoreExtensionError> {
            if !guard.active.load(Ordering::SeqCst) {
                return Err(ended_error(&owner));
            }
            Ok(())
        }
    };

    let all = {
        let sql_guard = sql_guard.clone();
        std::rc::Rc::new(
            move |columns: &[String],
                  statement: &str,
                  params: &[Value],
                  order_by: Option<&str>|
                  -> Result<Vec<SqlRow>, String> {
                sql_guard().map_err(|error| error.message)?;
                single_extension_statement(statement).map_err(|error| error.message)?;
                for column in columns {
                    single_extension_statement(column).map_err(|error| error.message)?;
                }
                let order = order_by
                    .map(|order| format!(" ORDER BY {order}"))
                    .unwrap_or_default();
                let sql = format!("{statement}{order}");
                ctx.all(&sql, &values_to_params(params))
                    .map_err(|error| error.message())
            },
        )
            as std::rc::Rc<
                dyn Fn(&[String], &str, &[Value], Option<&str>) -> Result<Vec<SqlRow>, String> + 'a,
            >
    };
    let one = {
        let sql_guard = sql_guard.clone();
        std::rc::Rc::new(
            move |columns: &[String],
                  statement: &str,
                  params: &[Value]|
                  -> Result<Option<SqlRow>, String> {
                sql_guard().map_err(|error| error.message)?;
                single_extension_statement(statement).map_err(|error| error.message)?;
                for column in columns {
                    single_extension_statement(column).map_err(|error| error.message)?;
                }
                ctx.one(statement, &values_to_params(params))
                    .map_err(|error| error.message())
            },
        )
            as std::rc::Rc<dyn Fn(&[String], &str, &[Value]) -> Result<Option<SqlRow>, String> + 'a>
    };
    let exec = {
        let sql_guard = sql_guard.clone();
        std::rc::Rc::new(
            move |statement: &str, params: &[Value]| -> Result<usize, String> {
                sql_guard().map_err(|error| error.message)?;
                single_extension_statement(statement).map_err(|error| error.message)?;
                ctx.run(statement, &values_to_params(params))
                    .map_err(|error| error.message())
            },
        ) as std::rc::Rc<dyn Fn(&str, &[Value]) -> Result<usize, String> + 'a>
    };
    let binding_for = {
        let sql_guard = sql_guard.clone();
        std::rc::Rc::new(
            move |lookup: LegacyBindingWhere| -> Result<Option<LegacyBinding>, String> {
                sql_guard().map_err(|error| error.message)?;
                legacy_binding_for(ctx, &lookup, now).map_err(|error| error.message())
            },
        )
            as std::rc::Rc<dyn Fn(LegacyBindingWhere) -> Result<Option<LegacyBinding>, String> + 'a>
    };

    ExtensionTransaction {
        tx: StoreExtensionTransaction {
            all,
            one,
            exec,
            binding_for,
        },
        guard,
    }
}

fn values_to_params(values: &[Value]) -> Vec<rusqlite::types::Value> {
    values
        .iter()
        .map(|value| match value {
            Value::Null => rusqlite::types::Value::Null,
            Value::Bool(flag) => rusqlite::types::Value::Integer(i64::from(*flag)),
            Value::Number(number) => number.as_i64().map_or_else(
                || rusqlite::types::Value::Real(number.as_f64().unwrap_or(0.0)),
                rusqlite::types::Value::Integer,
            ),
            Value::String(text) => rusqlite::types::Value::Text(text.clone()),
            other => rusqlite::types::Value::Text(other.to_string()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> StoreContext {
        let ctx = StoreContext::new(
            rusqlite::Connection::open_in_memory().expect("memory database"),
            StoreContext::test_config(),
            StoreContext::test_identity(),
        );
        for step in super::super::schema::GATEWAY_MIGRATIONS {
            for statement in *step {
                ctx.exec(statement).expect("migration step");
            }
        }
        ctx
    }

    fn seed_binding(ctx: &StoreContext, id: &str, status: &str, expires_at: Option<&str>) {
        ctx.run(
            "INSERT INTO bindings (binding_id, revision, status, platform, account_id, chat_id, thread_id, session_realm_id, session_durable_id, direction_inbound, direction_outbound, inbound_mode, outbound_events, policy_id, created_at, updated_at, lease_started_at, expires_at) VALUES (?, 1, ?, 'slack', 'acct', 'C1', ?, 'realm-1', 'B', 1, 1, 'auto', '[]', 'policy', '2026-11-25T00:00:00.000Z', '2026-11-25T00:00:00.000Z', '2026-11-25T00:00:00.000Z', ?)",
            &[
                v_str(id),
                v_str(status),
                v_str(format!("T-{id}")),
                super::super::store_ops::v_opt_str(expires_at),
            ],
        )
        .expect("seed binding");
    }

    fn lookup(id: &str) -> LegacyBindingWhere {
        LegacyBindingWhere {
            platform: "slack".to_owned(),
            account_id: "acct".to_owned(),
            chat_id: "C1".to_owned(),
            thread_id: format!("T-{id}"),
        }
    }

    #[test]
    fn a_second_statement_is_refused() {
        assert!(single_extension_statement("SELECT 1").is_ok());
        assert!(single_extension_statement("SELECT 1;").is_ok());
        assert!(single_extension_statement("SELECT 1; DROP TABLE bindings").is_err());
        assert!(single_extension_statement("SELECT ';'").is_ok());
    }

    #[test]
    fn binding_for_reads_only_an_active_unexpired_binding() {
        let ctx = context();
        seed_binding(&ctx, "live", "active", None);
        seed_binding(&ctx, "future", "active", Some("2099-01-01T00:00:00.000Z"));
        seed_binding(&ctx, "lapsed", "active", Some("2020-01-01T00:00:00.000Z"));
        seed_binding(&ctx, "closed", "detached", None);
        let scope = extension_transaction(&ctx, "alpha", 1_800_000_000_000);
        let live = (scope.tx.binding_for)(lookup("live")).expect("lookup");
        assert_eq!(
            live,
            Some(LegacyBinding {
                binding_id: "live".to_owned(),
                policy_id: "policy".to_owned(),
                session_durable_id: "B".to_owned(),
            })
        );
        assert!(
            (scope.tx.binding_for)(lookup("future"))
                .expect("lookup")
                .is_some()
        );
        assert_eq!(
            (scope.tx.binding_for)(lookup("lapsed")).expect("lookup"),
            None
        );
        assert_eq!(
            (scope.tx.binding_for)(lookup("closed")).expect("lookup"),
            None
        );
        assert_eq!(
            (scope.tx.binding_for)(lookup("missing")).expect("lookup"),
            None
        );
    }

    #[test]
    fn binding_for_answers_none_when_the_store_has_no_bindings_table() {
        let ctx = StoreContext::new(
            rusqlite::Connection::open_in_memory().expect("memory database"),
            StoreContext::test_config(),
            StoreContext::test_identity(),
        );
        let scope = extension_transaction(&ctx, "alpha", 1);
        assert_eq!((scope.tx.binding_for)(lookup("any")).expect("lookup"), None);
    }

    #[test]
    fn a_helper_is_refused_once_the_transaction_ended() {
        let ctx = context();
        let scope = extension_transaction(&ctx, "alpha", 1);
        scope.finish();
        let refused = (scope.tx.binding_for)(lookup("any")).expect_err("ended");
        assert!(refused.contains("has ended"), "{refused}");
        let refused = (scope.tx.exec)("SELECT 1", &[]).expect_err("ended");
        assert!(refused.contains("has ended"), "{refused}");
    }
}
