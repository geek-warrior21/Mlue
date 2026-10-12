# mlue — Engineering Agent Guidelines & Project Invariants

This document outlines the mandatory engineering standards, architectural invariants, and behavioral constraints for all AI agents and human contributors working on the **mlue** codebase.

## 1. Project Invariants

- **Local-First Architecture**: mlue is a desktop-native application designed to give users sovereignty over their AI workflows. It must run standalone without requiring external cloud accounts or servers.
- **No Required mlue Account**: Users must never be forced to sign up for an mlue account, authenticate with a centralized mlue service, or register a license key to use the core application.
- **No Mandatory Paid Cloud Infrastructure**: Local models and directly provided API credentials must function without any mandatory intermediate paid proxy or subscription.
- **Technology Stack**:
  - Desktop framework: Tauri 2
  - Backend: Rust + Axum + Tokio (embedded runtime)
  - Frontend: React + TypeScript + Vite + Tailwind CSS
- **Model-Provider Agnostic**: The internal backend architecture must remain strictly model-provider agnostic. Provider integrations (Ollama, OpenAI, Gemini, Claude, etc.) are adapters/drivers. OpenAI compatibility is an external API surface, not the internal core representation.
- **No Browser-Cookie or Undocumented AI-Provider Hacks**: Never reverse-engineer private web sessions, hijack browser cookies, or use fragile private scraping APIs to connect to AI services. Use only official, documented developer APIs and user-provided keys or local endpoints.
- **No Plaintext Secrets in Source Control**: Never commit credentials, tokens, or private keys. All secret persistence must adhere to secure credential storage boundaries.
- **Explicit Security Boundaries**:
  - The embedded HTTP server must bind exclusively to loopback (`127.0.0.1`). It must never bind to `0.0.0.0` or expose open network surfaces without explicit user configuration.
  - No permissive wildcard CORS (`*`).
  - No client-controlled endpoint may execute arbitrary OS shell commands or traverse the local filesystem.
- **No Unsolicited Feature Expansion**: Strictly adhere to the scope of the assigned milestone/brick. Do not add speculative dependencies or prematurely implement future roadmap items.
- **Empirical Validation**: All completed changes must be rigorously verified. Never claim tests passed or builds succeeded without actually executing the corresponding commands and inspecting results.

## 2. Coding Standards

- **Rust**:
  - Enforce idiomatic Rust: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`.
  - Handle errors gracefully using typed errors; do not panic or unwrap in production runtime paths.
  - Ensure server startup errors (such as port collisions) do not crash the Tauri desktop UI.
- **Frontend**:
  - Strict TypeScript (`tsc --noEmit` or `npm run build` must pass cleanly).
  - Use Tailwind CSS with clean semantic classes.
  - Rely on Tauri IPC (`invoke`) to retrieve verified backend state rather than assuming backend readiness.

## 3. Communication & Milestones

- Update `docs/PROGRESS.md` with every milestone.
- Document system design changes in `docs/ARCHITECTURE.md`.
- Report clear verification logs distinguishing `PASS`, `FAIL`, and `NOT RUN` with environmental context.
