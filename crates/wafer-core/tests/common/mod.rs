//! Shared fakes and helpers for the database handler integration test
//! suites. Each consuming test file does `mod common;` and reaches these via
//! `common::db_fakes::*`.

pub mod db_fakes;
