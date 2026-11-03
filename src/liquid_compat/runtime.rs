// SPDX-License-Identifier: AGPL-3.0-only
//! Rendering runtime with Ruby-Liquid nil semantics.
//!
//! liquid-rs raises `Unknown variable` on any unresolved path; Ruby Liquid
//! (what TRMNL cloud runs) resolves missing keys to nil and renders empty.
//! Community templates dereference `trmnl.*` and plugin fields freely, so a
//! strict runtime kills whole screens on one absent key. This wrapper turns
//! unresolved lookups into nil — parse errors and filter errors still raise.
#![allow(deprecated)] // the Runtime trait signatures use KString aliases

use liquid_core::model::{ScalarCow, Value, ValueCow};
use liquid_core::runtime::{PartialStore, Registers, Runtime};

pub struct LenientRuntime<R: Runtime> {
    inner: R,
}

impl<R: Runtime> LenientRuntime<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<R: Runtime> Runtime for LenientRuntime<R> {
    fn partials(&self) -> &dyn PartialStore {
        self.inner.partials()
    }

    fn name(&self) -> Option<liquid_core::model::KStringRef<'_>> {
        self.inner.name()
    }

    fn roots(&self) -> std::collections::BTreeSet<liquid_core::model::KStringCow<'_>> {
        self.inner.roots()
    }

    fn try_get(&self, path: &[ScalarCow<'_>]) -> Option<ValueCow<'_>> {
        self.inner.try_get(path)
    }

    fn get(&self, path: &[ScalarCow<'_>]) -> liquid_core::Result<ValueCow<'_>> {
        match self.inner.get(path) {
            Ok(v) => Ok(v),
            // Ruby Liquid: unknown variable renders as nil, not an error.
            Err(_) => Ok(ValueCow::Owned(Value::scalar(""))),
        }
    }

    fn set_global(&self, name: liquid_core::model::KString, val: Value) -> Option<Value> {
        self.inner.set_global(name, val)
    }

    fn set_index(&self, name: liquid_core::model::KString, val: Value) -> Option<Value> {
        self.inner.set_index(name, val)
    }

    fn get_index<'a>(&'a self, name: &str) -> Option<ValueCow<'a>> {
        self.inner.get_index(name)
    }

    fn registers(&self) -> &Registers {
        self.inner.registers()
    }
}
