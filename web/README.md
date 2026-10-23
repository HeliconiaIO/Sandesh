# Sandesh Web UI

The developer console. React 19 + TypeScript + Vite, embedded into the Sandesh binary at build time.

## Commands

```bash
npm install
npm run dev        # Vite on :5173, proxying /api and the WebSocket to :8080
npm run build      # writes dist/, which the Rust binary embeds
npm run typecheck
npm run lint
npm test           # component tests (vitest)
npm run e2e        # browser tests (playwright); starts a real Sandesh
```

For `npm run dev` to be useful, run `cargo run` from the repository root first. The proxy means no
component ever names a host or port: the API is same-origin at `/api` in both development and
production.

## Layout

```text
src/
├── api/          Transport. The typed client, the resource functions, the event stream
├── app/          The shell, routing and the error boundary
├── components/   Shared UI, including the sandboxed HTML preview
├── features/     mailboxes / messages / mime / settings
├── generated/    GENERATED — do not edit; see below
├── hooks/        Server state (queries) and event-driven invalidation
├── lib/          Formatting and clipboard helpers
├── routes/       One file per route
└── styles/       Design tokens and base styles
```

## Generated types

`src/generated/api.ts` is generated from `docs/openapi.json`, which is itself generated from the
Rust API. **Never edit it by hand.** To regenerate, from the repository root:

```bash
cargo xtask generate-api
```

CI fails if the committed output differs from what generation produces, so the UI cannot be typed
against an API that no longer exists.

## Conventions

**No hardcoded values.** Every colour, space, radius and duration is a token in
`src/styles/tokens.css`. Both themes define the same token names, so no component branches on the
theme. A literal `#1a1a1a` or `12px` in a component is a bug that only appears in the theme its
author was not using.

**No `fetch` in components.** All HTTP goes through `src/api/client.ts`, which owns error
translation and the RFC 9457 problem-details contract.

**No `new WebSocket` in components.** The event stream is `src/api/events.ts`, which owns
reconnection and backoff.

**No MIME parsing.** The backend is the only parser. The tree drawn here is its `MimePart` list and
the byte ranges highlighted are its ranges.

**Email HTML never touches the app DOM.** It renders in an iframe with an empty `sandbox`
attribute. Do not add `allow-scripts` or `allow-same-origin` — read the comment at the top of
`src/components/HtmlPreview.tsx` first.
