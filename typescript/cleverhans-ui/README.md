# @cleverhans/ui

Styled block pack and chat window for
[CleverHans](https://github.com/2commits/cleverhans) — the 10-minute happy
path over the headless
[`@cleverhans/react`](https://www.npmjs.com/package/@cleverhans/react),
never a core dependency.

```tsx
import { FloatingChat } from "@cleverhans/ui";
import "@cleverhans/ui/styles.css";

<FloatingChat session={session} />;
```

- `<AgentChat>` — inline chat surface; `<FloatingChat>` — the launcher +
  window variant.
- Default blocks for `confirm` and `bulk_preview`; pass `components` to
  add or override per block type (merged over the defaults).
- Theme via the `--ch-*` CSS custom properties in `styles.css`.
