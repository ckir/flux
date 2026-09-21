# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/ckir/flux/releases/tag/v0.1.0) - 2026-09-21

### Build and CI

- a release profile for the shipped binary: `opt-level = 3`, fat LTO, one codegen unit, `panic = "abort"`, symbols stripped ([#18](https://github.com/ckir/flux/pull/18))

### Miscellaneous

- scaffold the Rust workspace and dev toolchain
