/** True for Google consumer apps: com.google.android.* and Chrome. */
export function isGoogleApp(packageName) {
  return (
    packageName.startsWith("com.google.android.") ||
    packageName === "com.android.chrome"
  );
}
