/**
 * macOS-style traffic light window controls (close / minimize / zoom).
 * Placed top-left of the custom title bar; icons appear on hover.
 */

export function TrafficLights({ className = '' }: { className?: string }) {
  return (
    <div
      className={`titlebar-no-drag traffic-lights flex items-center gap-[8px] shrink-0 ${className}`}
      role="group"
      aria-label="Window"
    >
      <button
        type="button"
        className="traffic-light traffic-light--close"
        title="Close"
        aria-label="Close window"
        onClick={() => void window.windowControls?.close()}
      />
      <button
        type="button"
        className="traffic-light traffic-light--min"
        title="Minimize"
        aria-label="Minimize window"
        onClick={() => void window.windowControls?.minimize()}
      />
      <button
        type="button"
        className="traffic-light traffic-light--max"
        title="Maximize"
        aria-label="Maximize window"
        onClick={() => void window.windowControls?.maximize()}
      />
    </div>
  );
}
