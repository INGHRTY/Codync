/** The chat window's own geometry: the main process needs it for the window's minimum size, and the
 * renderer for folding the sidebar. One place, so the two can't drift apart. The sizes are content
 * pixels: the chat window's minimums use `useContentSize`, so the frame never eats into them. */

/** The conversation's own minimum (Grok Bot's chat min width), and the rail the sidebar folds to. */
export const CHAT_MIN_WIDTH = 424
export const SIDEBAR_RAIL = 48
/** The sidebar's drag edge (`.resize-handle` in components/controls.css), between it and the chat. */
export const SIDEBAR_HANDLE_WIDTH = 1

/** Narrowest content that still leaves the conversation its own minimum beside the rail and its edge. */
export const WINDOW_MIN_WIDTH = SIDEBAR_RAIL + SIDEBAR_HANDLE_WIDTH + CHAT_MIN_WIDTH
/** The same floor as `.chat-window`'s own `min-height` in chat-window.css. */
export const WINDOW_MIN_HEIGHT = 500
