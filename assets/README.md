# sigilbuzz brand assets

The sigilbuzz mark is a sigil: an S drawn as one gold stroke, with a small
ring at one end and a short bar crossing the other.

## Files

| File | Size | Use it for |
| --- | --- | --- |
| `banner.png` | 2560x1280 | The header image at the top of the README. |
| `social-preview.png` | 1280x640 | The GitHub social preview (see below). Also works for link cards and slides. |
| `icon.svg` | 512x512 | The app icon: the mark on a dark rounded tile. Use it for avatars, package registries, and documentation sites. |
| `icon-512.png` | 512x512 | The same icon as a PNG, for places that do not accept SVG. |
| `favicon-32.png` | 32x32 | A browser tab icon. |
| `mark-dark.svg` | 512x512 | The bare mark for dark backgrounds. The background is transparent. |
| `mark-light.svg` | 512x512 | The bare mark for light backgrounds. The background is transparent. |
| `lockup-dark.png` | 1384x352 | The mark with the `sigilbuzz` wordmark and tagline on a dark tile. |
| `lockup-light.png` | 1384x352 | The mark with the `sigilbuzz` wordmark and tagline on a light tile. |

To show the right bare mark for the reader's GitHub theme:

```html
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/mark-dark.svg">
  <img src="assets/mark-light.svg" alt="sigilbuzz" width="96">
</picture>
```

## Colors

| Name | Hex | Role |
| --- | --- | --- |
| Midnight | `#131B2E` | Dark ground. |
| Gold | `#D4AA55` | The S on dark backgrounds, and the accent color. |
| Parchment | `#EFE6D2` | The ring, the bar, and the wordmark on dark backgrounds. |
| Gold (light) | `#A47B24` | The S on light backgrounds. |
| Ink | `#141A2A` | The ring, the bar, and the wordmark on light backgrounds. |
| Vellum | `#F3EEE2` | Light ground. |

The wordmark is set in Source Sans Pro, weight 400, with slightly open
letter spacing. The lockups carry the tagline "The marks you meant." in the
same face.

## Social preview

GitHub does not read the social preview from the repository. To set it,
open the repository's Settings, and under General > Social preview, upload
`social-preview.png`.

## Source

The artwork is drawn in Penpot, in the Brands project, file "Repository
Brands". The mark is the "sigilbuzz mark" component with Concept "S1 Sigil
Line" and Ink "Colour". The banner is the sigilbuzz README banner board on
the Family page. Export from there if you need another size or format.
