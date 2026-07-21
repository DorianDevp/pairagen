import { Component, input } from "@angular/core";

@Component({
  selector: "app-title-card",
  template: `<h2>{{ title() }}</h2>`,
})
export class TitleCard {
  @Input() title!: string;
}
