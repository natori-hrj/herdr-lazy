# VHS recording review

The top image in each comparison is the previous 1760×660 recording with
20 px text. The bottom image is the revised 1280×720 recording with 22 px text.
Each image is scaled to the linked display width. The terminal frames are
unaltered apart from scaling for this comparison.

| View | README width | Phone width |
| --- | --- | --- |
| Manage | [800 px](comparison/before-after-manage-800.png) | [390 px](comparison/before-after-manage-390.png) |
| Marketplace | [800 px](comparison/before-after-marketplace-800.png) | [390 px](comparison/before-after-marketplace-390.png) |

The revised Manage list shows nine plugins instead of three. The filtered
Marketplace shows 12 results instead of three. At 800 px, the revised images
give the list, status labels, and shortcuts more room to be read. At 390 px,
the headings, selected result, and status labels are easier to distinguish.
Use the full-size image for commit IDs and secondary shortcuts.

Both 22 px and 24 px recordings used the same 1280×720 viewport, JetBrains
Mono, and Catppuccin Mocha. We selected 22 px because it keeps the complete
binding preview in the first Details screen and fits one more unfiltered
Marketplace row. At 24 px, the binding command needs scrolling. At both sizes
the filtered Marketplace fits all 12 results.

The final GIF and H.264 MP4 were decoded from start to finish and measured
28.36 seconds at 1280×720. Their sizes are 836 kB and 442 kB
(previously 545 kB and 314 kB). The longer workflow and denser catalog increase
file size. Use the MP4 to share the smaller version. Visual review included
full-size PNGs and frames from both videos. The search cursors, selected rows,
Unicode arrows, star ratings, and rules remained distinct. The
`Refreshing…` frame retains all 12 results, and Manage confirms that the theme
was saved.

## TUI follow-ups

- At this terminal width, Manage notes and Marketplace descriptions drop out
  while the name columns remain wide. Consider allocating width from visible
  content and offering a short description for the focused result.
- The Command Palette and Theme Selection put short lists at the top and
  their hints or color samples at the bottom. Consider moving related
  controls and previews closer to the focused item while preserving the
  same keyboard navigation.
- Dim metadata and footer hints are hard to read at phone preview size.
  Review the contrast of these secondary roles and avoid relying on color
  alone to show which theme was applied.
