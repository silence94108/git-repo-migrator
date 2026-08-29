/// <reference types="vite/client" />

// `import.meta.env.MODE` is what gates the E2E bridge seam in
// `src/state/migrationStore.ts`, so the Vite client types have to be part of the
// program the type checker sees — not just of the bundler's view of it.
