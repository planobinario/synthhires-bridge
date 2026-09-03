# synthhires-bridge

Desktop Daemon (Rust) + bridge móvil para SynthHires. La app web vive en el
repo separado `synth-hires`.

## Para agentes — lee esto al empezar

Todo el flujo de trabajo está en `apps/desktop-daemon/AGENTS.md`:

- Reglas de compilación (CI, no local) y verificación de las crates.
- Publicación de binarios (push a `main` → release `edge`; tag `v*` →
  release versionada).
- Nombres de asset que la web espera.
- Autenticación de GitHub CLI (`gh auth login --with-token`).
- Contrato de protocolo web↔daemon.

Espejo en GitHub: `planobinario/synth-hires-bridge`.
