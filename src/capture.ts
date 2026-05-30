import "./app.css";
import { mount } from "svelte";
import CaptureSelector from "./lib/CaptureSelector.svelte";

mount(CaptureSelector, { target: document.getElementById("app")! });
