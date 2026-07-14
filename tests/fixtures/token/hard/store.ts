import { SchedulerEvent, SchedulerState } from "./types";
import { reduce } from "./reducer";

export function applyAll(
  initial: SchedulerState,
  events: SchedulerEvent[]
): SchedulerState {
  let state = initial;
  for (let i = 0; i < events.length; i++) {
    state = reduce(state, events[i]);
  }
  return state;
}

export function emptyState(): SchedulerState {
  return { bookings: [] };
}
