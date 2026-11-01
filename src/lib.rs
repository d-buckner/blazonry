// SPDX-License-Identifier: AGPL-3.0-only
//! mntr: TRMNL-compatible BYOS server, no premium tiers, no Chrome.
//!
//! Render pipeline: Liquid template -> Screen tree (serde JSON) -> tiny-skia
//! pixmap -> 4-level dither -> 2-bit BMP (the wire format TRMNL devices want).
//!
//! The element/layout model deliberately mirrors GPUI's vocabulary
//! (flex containers, request_layout/paint) so a later swap to a patched
//! GPUI-with-software-backend is a drop-in, not a rewrite.

pub mod admin;
pub mod admin_provider;
pub mod api;
pub mod auth;
pub mod auth_core;
pub mod backend;
#[cfg(feature = "browser")]
pub mod browser;
pub mod bmp;
pub mod cache;
pub mod db;
pub mod dither;
pub mod ext_dev;
pub mod extensions;
pub mod firmware;
pub mod liquid_compat;
pub mod markup;
pub mod models;
pub mod playlists;
pub mod providers;
pub mod screens;
pub mod render;
pub mod screen;
pub mod template;

pub use screen::Screen;

/// TRMNL OG display geometry.
pub const WIDTH: usize = 800;
pub const HEIGHT: usize = 480;
