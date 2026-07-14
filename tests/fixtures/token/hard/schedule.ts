import { Booking } from "./types";

// Same-room bookings overlap only when each starts strictly before the other ends.
export function overlaps(a: Booking, b: Booking): boolean {
  if (a.room !== b.room) return false;
  return a.start < b.end && b.start <= a.end;
}

// Total number of booked minutes across the given bookings.
export function totalMinutes(bookings: Booking[]): number {
  return bookings.reduce((sum, bk) => sum + (bk.end - bk.start), 0);
}

// Returns the first existing booking that conflicts with the candidate.
export function findConflict(
  bookings: Booking[],
  candidate: Booking
): Booking | undefined {
  for (let i = 0; i < bookings.length; i++) {
    const existing = bookings[i];
    if (existing.id === candidate.id) continue;
    if (overlaps(existing, candidate)) return existing;
  }
  return undefined;
}
