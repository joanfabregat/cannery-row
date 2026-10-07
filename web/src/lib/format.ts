/**
 * Instants are stored in UTC and shown in the reader's own time zone and
 * language; numbers keep enough digits to compare close results.
 */

const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const dateOnly = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });
const number = new Intl.NumberFormat(undefined, { maximumSignificantDigits: 4 });

function parse(value: string | null | undefined): Date | null {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date;
}

export function formatDateTime(value: string | null | undefined): string {
  const date = parse(value);
  return date === null ? "—" : dateTime.format(date);
}

export function formatDate(value: string | null | undefined): string {
  const date = parse(value);
  return date === null ? "—" : dateOnly.format(date);
}

const utcDay = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeZone: "UTC" });

const DAY = /^\d{4}-\d{2}-\d{2}$/;

/**
 * A date from an imported history, shown as the source gave it: a day
 * (`2026-09-28`) as that day, whatever the reader's time zone, and an instant
 * as an instant, even one at midnight.
 */
export function formatHistoryDate(value: string | null | undefined): string {
  if (value && DAY.test(value)) {
    const day = parse(`${value}T00:00:00Z`);
    return day === null ? "—" : utcDay.format(day);
  }
  return formatDateTime(value);
}

export function formatNumber(value: number | null | undefined): string {
  return value === null || value === undefined || !Number.isFinite(value)
    ? "—"
    : number.format(value);
}

/** A difference with its sign, so "+0.02" and "−0.01" read at a glance. */
export function formatDelta(value: number | null | undefined): string {
  if (value === null || value === undefined || !Number.isFinite(value)) return "—";
  if (value === 0) return "0";
  return value > 0 ? `+${number.format(value)}` : `−${number.format(-value)}`;
}

const units = {
  minute: new Intl.NumberFormat(undefined, { style: "unit", unit: "minute", unitDisplay: "long" }),
  hour: new Intl.NumberFormat(undefined, { style: "unit", unit: "hour", unitDisplay: "long" }),
  day: new Intl.NumberFormat(undefined, { style: "unit", unit: "day", unitDisplay: "long" }),
};

/**
 * How long ago an instant was, in whole minutes, hours or days: "2 hours".
 * Under two hours it counts minutes, under two days hours.
 */
export function formatWaited(since: string, now: number = Date.now()): string {
  const date = parse(since);
  if (date === null) return "—";
  const minutes = Math.max(1, Math.floor((now - date.getTime()) / 60_000));
  if (minutes < 120) return units.minute.format(minutes);
  const hours = Math.floor(minutes / 60);
  if (hours < 48) return units.hour.format(hours);
  return units.day.format(Math.floor(hours / 24));
}

/** A plain-text excerpt: the first `max` characters, cut at a word. */
export function excerpt(text: string, max = 160): string {
  const flat = text.replace(/\s+/g, " ").trim();
  if (flat.length <= max) return flat;
  const cut = flat.slice(0, max);
  const space = cut.lastIndexOf(" ");
  return `${(space > max / 2 ? cut.slice(0, space) : cut).trimEnd()}…`;
}
