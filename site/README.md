# Spaghetti documentation site

Static product docs for GitHub Pages. No build step.

| File | Role |
|---|---|
| `index.html` | Landing page — multi-agent product story, three-plane architecture, CLI mocks, SDK |
| `api.html` | SDK API reference (multi-source, SourceFilter, live, runtime) |
| `commands.html` | Full CLI command reference |
| `styles.css` | Shared design system (dark + light) |
| `app.js` | Theme toggle, nav, copy, accordion, scroll-spy |
| `.nojekyll` | Disable Jekyll processing on GitHub Pages |

**Aligned with RFC 011:** one Rust observation/query owner for Claude Code,
OpenAI Codex, and Grok; async client-backed SDK reads; no production
TypeScript ingest fallback.

Terminal product shots on the landing page are **HTML mocks** styled to match
real CLI chrome (window dots, mono layout, FTS snippets). They use synthetic
project names and numbers only — no live `~/.claude` / `~/.codex` data.

## Preview locally

```bash
# from repo root
npx --yes serve site
# or
python3 -m http.server 8080 --directory site
```

Open `http://localhost:3000` (or `:8080`).

## Publish on GitHub Pages

### Option A — Deploy `/docs` folder (simplest)

Copy (or symlink) this directory’s contents into the repo `docs/` root if you
prefer the built-in “Deploy from branch → `/docs`” source. Keep engineering
RFCs under `docs/rfcs/`.

### Option B — GitHub Actions from `site/`

In the repo **Settings → Pages → Build and deployment**, choose **GitHub Actions**,
then add a workflow such as:

```yaml
# .github/workflows/pages.yml
name: Deploy docs
on:
  push:
    branches: [main]
    paths: ['site/**']
  workflow_dispatch:

permissions:
  contents: read
  pages: write
  id-token: write

concurrency:
  group: pages
  cancel-in-progress: true

jobs:
  deploy:
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/upload-pages-artifact@v3
        with:
          path: site
      - id: deployment
        uses: actions/deploy-pages@v4
```

### Option C — `gh-pages` branch

```bash
# one-shot publish of site/ to gh-pages branch
npx --yes gh-pages -d site
```

## Design notes

- Terminal-craft dark theme (ink surfaces, teal accent) aligned with the CLI cyan theme
- Multi-agent palette: teal for Claude, magenta for Codex in mocks
- Self-contained SVG architecture diagram (no Mermaid runtime dependency)
- Instrument Sans + JetBrains Mono
- Zero framework; works offline once fonts are cached

## Keeping docs current

When shipping product changes, update:

1. Version strings (`v0.5.x`) on landing + API/commands headers
2. `createObservationService` options and async query signatures in `api.html`
3. CLI mocks (Agent column, tagline, multi-agent TUI tree) in `index.html`
4. New commands or multi-source behavior in `commands.html`
