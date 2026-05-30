import "./app.css";
import { mount } from "svelte";
import Overlay from "./lib/Overlay.svelte";

mount(Overlay, { target: document.getElementById("app")! });
