# Rift — Color Identity

> Brand identity and design system reference for the Rift platform.

---

## Brand Direction

**Vibe:** Clean, modern, pastel-forward — light mode only.
**Personality:** Approachable but production-grade. Soft without being weak.
**Differentiator:** Lavender/violet in a space dominated by black (Vercel) and blue (AWS, Docker).

---

## Color Palette

### Primary — Violet

The core brand color. Used for primary actions, active states, and brand expression.

| Token           | Hex       | OKLCh                      | Usage                        |
| --------------- | --------- | --------------------------- | ---------------------------- |
| `primary`       | `#7C5CFC` | `oklch(0.55 0.27 280)`     | Buttons, links, focus rings  |
| `primary-soft`  | `#C4B5FD` | `oklch(0.80 0.12 280)`     | Soft badges, selected states |
| `primary-hover` | `#6D28D9` | `oklch(0.42 0.27 285)`     | Button hover, pressed links  |

### Violet Scale (full)

For when you need fine-grained control within the primary family.

| Step | Hex       | Usage                    |
| ---- | --------- | ------------------------ |
| 50   | `#F5F3FF` | Surfaces, card fills     |
| 100  | `#EDE9FE` | Hover backgrounds        |
| 200  | `#DDD6FE` | Active backgrounds       |
| 300  | `#C4B5FD` | Soft accent, badges      |
| 400  | `#A78BFA` | Muted accent text        |
| 500  | `#8B5CF6` | Secondary emphasis       |
| 600  | `#7C5CFC` | **Primary**              |
| 700  | `#6D28D9` | Hover / pressed          |
| 800  | `#5B21B6` | Deep accent              |
| 900  | `#1E1B4B` | Headings, foreground     |

### Accent — Rose Pink

Secondary accent for highlights, badges, and special indicators. Analogous to violet on the color wheel.

| Token           | Hex       | Usage                             |
| --------------- | --------- | --------------------------------- |
| `accent`        | `#F472B6` | Badges, notification dots, hover  |
| `accent-soft`   | `#FCE7F3` | Soft background for accent badges |
| `accent-surface`| `#FDF2F8` | Subtle surface tint               |

### Neutrals

| Token        | Hex       | Usage                        |
| ------------ | --------- | ---------------------------- |
| `background` | `#FFFFFF` | Page background              |
| `foreground` | `#1E1B4B` | Primary text (deep indigo)   |
| `muted`      | `#6B7280` | Secondary / body text        |
| `border`     | `#E9E5FF` | Card and input borders       |
| `surface`    | `#F5F3FF` | Elevated surfaces, cards     |
| `divider`    | `#E5E7EB` | Neutral dividers, separators |

### Semantic

Standard functional colors. Do not use these for branding — only for communicating state.

| Token        | Hex       | Usage                          |
| ------------ | --------- | ------------------------------ |
| `success`    | `#10B981` | Deployed, build passed, online |
| `warning`    | `#F59E0B` | Attention, degraded, building  |
| `error`      | `#EF4444` | Failed, offline, destructive   |
| `info`       | `#7C5CFC` | Reuse primary for info states  |

---

## Typography

### Fonts

| Role     | Family            | Source                   | Weights        |
| -------- | ----------------- | ------------------------ | -------------- |
| Sans     | **Satoshi**       | fontshare.com (free)     | 300–900        |
| Mono     | **JetBrains Mono**| Google Fonts (free)      | 400, 500, 700  |

### Scale

| Token   | Size     | Weight   | Line Height | Usage                    |
| ------- | -------- | -------- | ----------- | ------------------------ |
| `h1`    | 1.875rem | Bold 700 | 1.2         | Page titles              |
| `h2`    | 1.5rem   | Semi 600 | 1.25        | Section headings         |
| `h3`    | 1.25rem  | Semi 600 | 1.3         | Card headings            |
| `body`  | 0.875rem | Reg 400  | 1.5         | Body text, descriptions  |
| `small` | 0.75rem  | Med 500  | 1.4         | Labels, captions, badges |
| `code`  | 0.8125rem| Reg 400  | 1.6         | Code blocks, CLI output  |

---

## Shape

| Token         | Value      | Usage                          |
| ------------- | ---------- | ------------------------------ |
| `radius-sm`   | `0.25rem`  | Badges, small chips            |
| `radius`      | `0.375rem` | Buttons, inputs, cards         |
| `radius-md`   | `0.5rem`   | Dialogs, dropdowns             |
| `radius-lg`   | `0.75rem`  | Large cards, panels            |
| `radius-full` | `9999px`   | Avatars, pills, toggles        |

---

## Shadows

Light, violet-tinted shadows to match the palette.

| Token       | Value                                        | Usage              |
| ----------- | -------------------------------------------- | ------------------ |
| `shadow-sm` | `0 1px 2px oklch(0.55 0.05 280 / 0.06)`     | Inputs, small cards|
| `shadow`    | `0 2px 8px oklch(0.55 0.05 280 / 0.08)`     | Cards, dropdowns   |
| `shadow-lg` | `0 8px 24px oklch(0.55 0.05 280 / 0.12)`    | Dialogs, modals    |

---

## Usage Rules

1. **Primary violet is for actions.** Buttons, links, focus rings. Never use it for large background fills.
2. **Rose pink is for accents only.** Badges, notification dots, highlights. It should never compete with violet for attention.
3. **Surfaces are violet-tinted white**, not pure gray. `#F5F3FF` over `#F9FAFB`.
4. **Borders are violet-tinted** (`#E9E5FF`), not neutral gray. This keeps the palette cohesive.
5. **Text uses deep indigo** (`#1E1B4B`) not pure black. Softer on the eyes, stays in the violet family.
6. **Semantic colors are standard.** Green = success, amber = warning, red = error. No custom interpretations.
7. **No dark mode.** The app is light mode only. Design all components accordingly.

---

## Spacing

Comfortable density, standard 4px grid.

| Token | Value  |
| ----- | ------ |
| `1`   | 0.25rem|
| `2`   | 0.5rem |
| `3`   | 0.75rem|
| `4`   | 1rem   |
| `5`   | 1.25rem|
| `6`   | 1.5rem |
| `8`   | 2rem   |
| `10`  | 2.5rem |
| `12`  | 3rem   |
| `16`  | 4rem   |
