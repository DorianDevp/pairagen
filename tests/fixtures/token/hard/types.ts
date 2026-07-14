export interface Booking {
  id: string;
  room: string;
  start: number;
  end: number;
}

export interface BookEvent {
  kind: "book";
  id: string;
  room: string;
  start: number;
  end: number;
}

export interface CancelEvent {
  kind: "cancel";
  id: string;
}

export interface RescheduleEvent {
  kind: "reschedule";
  id: string;
  start: number;
  end: number;
}

export type SchedulerEvent = BookEvent | CancelEvent;

export interface SchedulerState {
  bookings: Booking[];
}
