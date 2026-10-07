/**
 * Markdown as one line of plain text, for a summary that shows it inline:
 * images and links become their text, emphasis, code and heading, quote and
 * list markers go, and whitespace collapses. Raw HTML tags are dropped.
 */
export function plainText(markdown: string): string {
  return (
    markdown
      .replace(/<\/?[A-Za-z][^>]*>/g, "")
      .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
      .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
      .replace(/\[([^\]]*)\]\[[^\]]*\]/g, "$1")
      .replace(/^\s{0,3}(?:#{1,6}\s+|>\s?|[-*+]\s+|\d+[.)]\s+)/gm, "")
      .replace(/`+([^`]*)`+/g, "$1")
      .replace(/(\*{1,3}|~~)(\S(?:.*?\S)?)\1/g, "$2")
      // Underscores only around words: `snake_case` stays.
      .replace(/(?<!\w)(_{1,3})(\S(?:.*?\S)?)\1(?!\w)/g, "$2")
      .replace(/\s+/g, " ")
      .trim()
  );
}
