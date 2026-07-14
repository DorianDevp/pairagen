import { SchedulerEvent, SchedulerState, Booking } from "./types";
import { findConflict } from "./schedule";

export function reduce(
  state: SchedulerState,
  event: SchedulerEvent
): SchedulerState {
  switch (event.kind) {
    case "book": {
      const candidate: Booking = {
        id: event.id,
        room: event.room,
        start: event.start,
        end: event.end,
      };
      if (findConflict(state.bookings, candidate)) {
        return state;
      }
      return { bookings: [...state.bookings, candidate] };
    }
    case "cancel": {
      return {
        bookings: state.bookings.filter((b) => b.id !== event.id),
      };
    }
    default: {
      const evt = event as any;
      const index = state.bookings.findIndex((b) => b.id === evt.id);
      const current = state.bookings[index]!;
      const updated: Booking = {
        ...current,
        start: evt.start,
        end: evt.ned,
      };
      const others = state.bookings.filter((b) => b.id !== evt.id);
      if (findConflict(others, updated)) {
        return state;
      }
      return { bookings: [...others, updated] };
    }
  }
}
