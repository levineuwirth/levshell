// Sparkline — compact trend line for telemetry widgets (spec §2.3.1).
//
// `values` is an array of numbers in `[0, maxValue]`, oldest first.
// Drawn as a baseline-anchored polyline; the area under it is lightly
// filled so the trend reads at a glance at bar size. No axes/labels —
// it's a glanceable spark, not a chart.

import QtQuick
import ".."

Canvas {
    id: spark

    property var values: []
    property real maxValue: 100
    property color lineColor: Theme.primary
    property real lineWidth: 1.5

    implicitWidth: 40
    implicitHeight: Theme.iconSize

    onValuesChanged: requestPaint()
    onLineColorChanged: requestPaint()
    onWidthChanged: requestPaint()
    onHeightChanged: requestPaint()

    onPaint: {
        const ctx = getContext("2d");
        ctx.reset();
        const n = values ? values.length : 0;
        if (n < 2) return;

        const w = width;
        const h = height;
        const cap = maxValue > 0 ? maxValue : 1;
        // x step across the full width; y inverted (0 at bottom).
        const dx = w / (n - 1);
        function px(i) { return i * dx; }
        function py(v) {
            const c = Math.max(0, Math.min(cap, v));
            return h - (c / cap) * (h - lineWidth) - lineWidth / 2;
        }

        // Filled area under the curve.
        ctx.beginPath();
        ctx.moveTo(0, h);
        for (let i = 0; i < n; i++) ctx.lineTo(px(i), py(values[i]));
        ctx.lineTo(w, h);
        ctx.closePath();
        ctx.fillStyle = Qt.rgba(lineColor.r, lineColor.g, lineColor.b, 0.14);
        ctx.fill();

        // The line itself.
        ctx.beginPath();
        ctx.moveTo(0, py(values[0]));
        for (let i = 1; i < n; i++) ctx.lineTo(px(i), py(values[i]));
        ctx.lineWidth = lineWidth;
        ctx.strokeStyle = lineColor;
        ctx.lineJoin = "round";
        ctx.stroke();
    }
}
