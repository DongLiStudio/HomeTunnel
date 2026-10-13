# HomeTunnel
<!-- impeccable:product-schema 1 -->

## Platform
adaptive

## Stack
User-approved Rust + Tauri 2; shared core and shared operations UI, native material adapters.

## Users
People who use private web services through SSH and value clear security boundaries, including the requesting user's Windows and Mac clients.

## Product Purpose
Open a private service from a portal, reuse or establish an Agent-authenticated loopback SSH tunnel, and return to the source browser without opening the manager window.

## Capabilities and Constraints
Agent/public-key authentication only, no password login, saved password, exported private keys, arbitrary commands, local service catalogue, or private path files. Portal links supply complete structured connection parameters. Both platforms offer per-connection open/renew/disconnect, disconnect all, configurable TTL, local-host detection, deduplication, startup checks, tray/menu-bar background execution, installation and clean uninstallation.

## Brand Commitments
Product name HomeTunnel / 家庭隧道, publisher DongLiStudio. A clear, refined system interface; real Apple Liquid Glass where supported, native Windows material. One function set, platform-appropriate system conventions.

## Evidence on Hand
Earlier Windows/Mac implementations and user-confirmed browser flows exist in the originating assistant project. None of its hostnames, IPs, private paths, logs, screenshots, credentials or telemetry belongs in this public repository.

## Product Principles
- Own and terminate only provably owned processes.
- Preserve host-key validation and Agent signing policy.
- Explain actionable failures without exposing full private URLs.
- Repair only this application's registration, respecting saved startup preferences.
- Real installed desktop acceptance is distinct from test and tooling success.
