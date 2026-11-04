import { invoke } from "@tauri-apps/api/core";
import { safeLocalStorage } from "../storage";
import { STORAGE_KEYS } from "@/config";

// Helper function to check if Clocks API should be used
export async function shouldUseClocksAPI(): Promise<boolean> {
  try {
    // Check if Clocks API is enabled in localStorage
    const clocksApiEnabled =
      safeLocalStorage.getItem(STORAGE_KEYS.CLOCKS_API_ENABLED) === "true";
    if (!clocksApiEnabled) return false;

    // Check if license is available
    const hasLicense = await invoke<boolean>("check_license_status");
    return hasLicense;
  } catch (error) {
    console.warn("Failed to check Clocks API availability:", error);
    return false;
  }
}
