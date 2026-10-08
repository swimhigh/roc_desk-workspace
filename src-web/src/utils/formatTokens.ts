/** Token-count display: below 10,000, show the raw integer; at/above
 * 10,000, show "x.xx万" (ten-thousands) -- token counts are an "about how
 * much was used" magnitude, no need for exact precision. */
export function formatTokenCount(n: number): string {
  if (n < 10000) return String(n);
  return `${(n / 10000).toFixed(2)}万`;
}
