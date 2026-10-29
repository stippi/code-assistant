# Docs site

The user documentation published at <https://stippi.github.io/code-assistant/>,
built with [Hugo](https://gohugo.io) (extended, 0.158 or newer) and deployed by
`.github/workflows/pages.yml` on every push to `main` that touches `site/`.

The developer notes in `/docs` are a different thing: design documents for
people working on the code. This site is for people using the app.

## Content and rendering are separate

| Path | What it is |
|---|---|
| `content/docs/**.md` | All guide text, as Markdown with a little front matter |
| `data/home.yaml` | All text of the landing page, including the window tour |
| `layouts/`, `assets/` | The theme: templates, CSS, JavaScript. No content |

Editing the documentation means editing Markdown or YAML. The templates only
need to change for new kinds of blocks.

## Preview

```bash
cd site
hugo server            # http://localhost:1313/code-assistant/
```

## Writing a page

Front matter:

```yaml
---
title: Decide what the agent may do without asking   # page heading
linkTitle: Permissions and sandbox                   # shorter name in the nav (optional)
weight: 40                                           # order within the section
lede: One or two sentences under the heading.
frontends: [app, tui, acp]                           # shown as availability pills
only: app                                            # optional nav badge for app-only pages
---
```

Sections are the folders under `content/docs/`, ordered by the `weight` in
their `_index.md`. Each section belongs to one of the three areas in the top
bar, set by `area: guides | configure | reference` in its `_index.md`. The
sidebar and the previous/next links only show the current area. Link to other pages with root-relative paths such as
`/docs/work/permissions/`. The base path is added when the site is built.

### Shortcodes

The guides cover three frontends. Wrap the steps that differ in tabs. The
visitor's choice (App / Terminal / Editor) applies to the whole site and is
remembered:

```text
{{< frontends >}}
{{< fe "app" >}}Markdown for the app{{< /fe >}}
{{< fe "tui" >}}Markdown for the terminal{{< /fe >}}
{{< fe "acp" >}}Markdown for ACP editors{{< /fe >}}
{{< /frontends >}}
```

A frontend without an `fe` block shows "Not available in this frontend", or
the text given as `{{< frontends missing="…" >}}`. A single `{{< fe "app" >}}`
without the tabs shows only for visitors who picked that frontend.

| Shortcode | Renders |
|---|---|
| `{{< step 1 "Title" >}}…{{< /step >}}` | A numbered step |
| `{{< callout tip "Title" >}}…{{< /callout >}}` | A note box. Kinds: `note`, `tip`, `warn` |
| `{{< term >}}…{{< /term >}}` | A terminal transcript. `[dim]…[/]`, `[acc]…[/]`, `[amb]…[/]` color parts of it |
| `{{< selectors on="Tools" />}}` | The app's selector row, with one selector highlighted. Optional inner HTML is shown below it, for a menu |
| `{{< permission-request text="…" >}}` | The app's permission banner |
| `{{< tour >}}` | The annotated window drawing from the landing page, as a numbered list |

Keep the wording of buttons and menus identical to the app, so readers can
find them. When the app changes, the matching page should change in the same
pull request.

## Enabling Pages

Once, in the repository settings: **Settings → Pages → Build and deployment →
Source: GitHub Actions**.
