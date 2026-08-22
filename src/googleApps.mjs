/** True for Google consumer apps: com.google.android.* and Chrome. */
export function isGoogleApp(packageName) {
  if (typeof packageName !== "string") return false;
  return (
    packageName.startsWith("com.google.android.") ||
    packageName === "com.android.chrome"
  );
}
