//! Environment / scope-chain helpers: name definition, lookup, reassignment,
//! and finding the root of a chain. These operate on the [`Env`] alias defined
//! in [`super::value`].

use std::rc::Rc;

use super::*;

/// Insert (or overwrite) a binding in *this* scope frame.
pub(crate) fn env_define(env: &Env, name: &str, value: Value, is_val: bool) {
    env.borrow_mut()
        .vars
        .insert(name.to_string(), Binding { value, is_val });
}

/// Resolve a name, walking the parent chain. Returns the bound value.
pub(crate) fn env_get(env: &Env, name: &str) -> Option<Value> {
    let scope = env.borrow();
    if let Some(b) = scope.vars.get(name) {
        return Some(b.value.clone());
    }
    let parent = scope.parent.clone();
    drop(scope);
    match parent {
        Some(p) => env_get(&p, name),
        None => None,
    }
}

/// Reassign an existing name (walking parents). Returns:
/// - `Ok(true)`  — reassigned successfully.
/// - `Ok(false)` — name not found anywhere.
/// - `Err(())`   — found but bound as `val` (immutable).
pub(crate) fn env_assign(env: &Env, name: &str, value: Value) -> Result<bool, ()> {
    let mut scope = env.borrow_mut();
    if let Some(b) = scope.vars.get_mut(name) {
        if b.is_val {
            return Err(());
        }
        b.value = value;
        return Ok(true);
    }
    let parent = scope.parent.clone();
    drop(scope);
    match parent {
        Some(p) => env_assign(&p, name, value),
        None => Ok(false),
    }
}

/// The root (outermost) env of a chain — used so methods see top-level names.
pub(crate) fn root_of(env: &Env) -> Env {
    let parent = env.borrow().parent.clone();
    match parent {
        Some(p) => root_of(&p),
        None => Rc::clone(env),
    }
}
