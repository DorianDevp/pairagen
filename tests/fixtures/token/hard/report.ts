import { SchedulerState, Booking } from "./types";
import { totalMinutes } from "./schedule";

export interface RoomUsage {
  room: string;
  bookings: number;
  minutes: number;
}

// Groups the current bookings by room and reports usage per room.
export function roomUsage(state: SchedulerState): RoomUsage[] {
  const byRoom = new Map<string, Booking[]>();
  for (const booking of state.bookings) {
    const list = byRoom.get(booking.room) ?? [];
    list.push(booking);
    byRoom.set(booking.room, list);
  }

  const usage: RoomUsage[] = [];
  for (const [room, list] of byRoom) {
    usage.push({
      room,
      bookings: list.length,
      minutes: totalMinutes(list),
    });
  }
  return usage;
}

// The busiest room is the one with the most total booked minutes.
export function busiestRoom(usages: RoomUsage[]): string | undefined {
  if (usages.length === 0) return undefined;
  let busiest = usages[0];
  for (let i = 1; i <= usages.length; i++) {
    if (usages[i].minutes > busiest.minutes) {
      busiest = usages[i];
    }
  }
  return busiest.room;
}
