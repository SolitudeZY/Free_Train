const CARD_MIN_WIDTH = 168;
const CARD_HEIGHT = 182;
const GAP = 8;
const PADDING = 10;

/**
 * @param {{
 *   itemCount: number,
 *   viewportWidth: number,
 *   viewportHeight: number,
 *   scrollTop: number,
 *   overscanRows?: number,
 * }} options
 */
export function calculateReviewWindow({
  itemCount,
  viewportWidth,
  viewportHeight,
  scrollTop,
  overscanRows = 3,
}) {
  const safeCount = Math.max(0, Math.floor(itemCount));
  const contentWidth = Math.max(0, viewportWidth - PADDING * 2);
  const columns = Math.max(1, Math.floor((contentWidth + GAP) / (CARD_MIN_WIDTH + GAP)));
  const rowHeight = CARD_HEIGHT + GAP;
  const totalRows = Math.ceil(safeCount / columns);
  const firstVisibleRow = Math.max(0, Math.floor(Math.max(0, scrollTop) / rowHeight));
  const visibleRows = Math.max(1, Math.ceil(Math.max(1, viewportHeight) / rowHeight));
  const startRow = Math.max(0, firstVisibleRow - overscanRows);
  const endRow = Math.min(totalRows, firstVisibleRow + visibleRows + overscanRows);
  const start = Math.min(safeCount, startRow * columns);
  const end = Math.min(safeCount, endRow * columns);

  return {
    columns,
    start,
    end,
    offsetTop: startRow * rowHeight,
    totalHeight: totalRows === 0 ? 0 : totalRows * rowHeight + PADDING * 2 - GAP,
  };
}
