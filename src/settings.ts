import "./app.css";
import { mount } from "svelte";
import Settings from "./lib/Settings.svelte";

mount(Settings, { target: document.getElementById("app")! });
