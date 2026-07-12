import { useEffect, useMemo, useRef } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import type { MetricSummary } from "@/api/types";
import { formatInteger, formatMetricValue, formatNumber } from "@/lib/format";

interface PlotSeries {
  label: string;
  values: (number | null)[];
  stroke: string;
  fill?: string;
  bars?: boolean;
  points?: boolean;
  width?: number;
}

interface PlotProjection {
  ariaLabel: string;
  xLabel: string;
  xFormat: "index" | "metric";
  yLabel: string;
  yFormat: "metric" | "integer" | "probability";
  x: number[];
  series: PlotSeries[];
}

function finiteOrNull(value: number | null): number | null {
  return value !== null && Number.isFinite(value) ? value : null;
}

function projection(metric: MetricSummary): PlotProjection[] {
  const { distribution } = metric.statistics;
  if (distribution.kind !== "histogram_ecdf") return [];

  const histogramX = distribution.histogram.counts.map((_, index) => {
    const lower = distribution.histogram.edges[index] ?? 0;
    const upper = distribution.histogram.edges[index + 1] ?? lower;
    return lower + (upper - lower) / 2;
  });
  return [
    {
      ariaLabel: `${metric.identity.id} backend-projected histogram`,
      xLabel: metric.identity.unit,
      xFormat: "metric",
      yLabel: "Count",
      yFormat: "integer",
      x: histogramX,
      series: [{
        label: "Bin count",
        values: distribution.histogram.counts,
        stroke: "#175cd3",
        fill: "rgba(23, 92, 211, 0.24)",
        bars: true,
        width: 1,
      }],
    },
    {
      ariaLabel: `${metric.identity.id} backend-projected empirical cumulative distribution`,
      xLabel: metric.identity.unit,
      xFormat: "metric",
      yLabel: "Cumulative probability",
      yFormat: "probability",
      x: distribution.ecdf.map((point) => point.value),
      series: [{
        label: "ECDF",
        values: distribution.ecdf.map((point) => point.cumulative_probability),
        stroke: "#16845b",
        points: true,
        width: 2,
      }],
    },
  ];
}

function RawDistributionPlot({ metric }: { metric: MetricSummary }) {
  const distribution = metric.statistics.distribution;
  if (distribution.kind !== "raw_points") return null;

  const minimum = finiteOrNull(metric.statistics.minimum);
  const maximum = finiteOrNull(metric.statistics.maximum);
  if (minimum === null || maximum === null) return null;
  const padding = minimum === maximum ? Math.max(Math.abs(minimum) * 0.01, 1) : 0;
  const domainMinimum = minimum - padding;
  const domainMaximum = maximum + padding;
  const plotLeft = 72;
  const plotRight = 956;
  const x = (value: number | null) => {
    const finite = finiteOrNull(value);
    if (finite === null) return null;
    return plotLeft + ((finite - domainMinimum) / (domainMaximum - domainMinimum)) * (plotRight - plotLeft);
  };
  const p25 = x(metric.statistics.p25);
  const p75 = x(metric.statistics.p75);
  const median = x(metric.statistics.median);
  const interval = metric.statistics.median_confidence_interval;
  const intervalLower = x(interval?.lower ?? null);
  const intervalUpper = x(interval?.upper ?? null);
  const jitter = [-18, 0, 18, -9, 9] as const;

  return (
    <div
      className="scientific-plot raw-distribution-plot"
      role="img"
      aria-label={`${metric.identity.id} horizontal box plot, deterministic beeswarm observations, median, and median confidence interval`}
      tabIndex={0}
    >
      <svg viewBox="0 0 1000 200" aria-hidden="true" focusable="false">
        <line className="plot-axis" x1={plotLeft} x2={plotRight} y1="176" y2="176" />
        <line className="box-whisker" x1={x(minimum) ?? plotLeft} x2={x(maximum) ?? plotRight} y1="70" y2="70" />
        <line className="box-whisker" x1={x(minimum) ?? plotLeft} x2={x(minimum) ?? plotLeft} y1="58" y2="82" />
        <line className="box-whisker" x1={x(maximum) ?? plotRight} x2={x(maximum) ?? plotRight} y1="58" y2="82" />
        {p25 !== null && p75 !== null ? (
          <rect className="box-quartiles" x={Math.min(p25, p75)} y="50" width={Math.max(Math.abs(p75 - p25), 1)} height="40" />
        ) : null}
        {median !== null ? <line className="box-median" x1={median} x2={median} y1="48" y2="92" /> : null}
        {intervalLower !== null && intervalUpper !== null ? (
          <>
            <line className="box-confidence" x1={intervalLower} x2={intervalUpper} y1="28" y2="28" />
            <line className="box-confidence" x1={intervalLower} x2={intervalLower} y1="21" y2="35" />
            <line className="box-confidence" x1={intervalUpper} x2={intervalUpper} y1="21" y2="35" />
          </>
        ) : null}
        {distribution.values.map((value, index) => {
          const pointX = x(value);
          return pointX === null ? null : (
            <circle
              key={index}
              className={metric.statistics.outlier_indices.includes(index) ? "beeswarm-point beeswarm-outlier" : "beeswarm-point"}
              cx={pointX}
              cy={130 + jitter[index % jitter.length]}
              r="6"
            />
          );
        })}
        <text className="plot-label" x={plotLeft} y="196" textAnchor="start">{formatMetricValue(minimum, metric.identity.unit)}</text>
        <text className="plot-label" x={plotRight} y="196" textAnchor="end">{formatMetricValue(maximum, metric.identity.unit)}</text>
        <text className="plot-label" x="16" y="74">Box</text>
        <text className="plot-label" x="16" y="134">Raw</text>
      </svg>
    </div>
  );
}

function ScientificPlot({ metric, value }: { metric: MetricSummary; value: PlotProjection }) {
  const hostRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return undefined;

    const width = Math.max(280, Math.floor(host.getBoundingClientRect().width || 640));
    const data: uPlot.AlignedData = [
      value.x,
      ...value.series.map((series) => series.values),
    ];
    const chart = new uPlot({
      width,
      height: 240,
      legend: { show: true },
      cursor: { drag: { x: false, y: false } },
      axes: [
        {
          label: value.xLabel,
          stroke: "#475467",
          grid: { stroke: "#e4e7ec" },
          values: (_plot, ticks) => ticks.map((tick) => value.xFormat === "metric"
            ? formatMetricValue(tick, metric.identity.unit)
            : formatInteger(tick)),
        },
        {
          label: value.yLabel,
          stroke: "#475467",
          grid: { stroke: "#e4e7ec" },
          values: (_plot, ticks) => ticks.map((tick) => {
            if (value.yFormat === "metric") return formatMetricValue(tick, metric.identity.unit);
            if (value.yFormat === "probability") return formatNumber(tick, 3);
            return formatInteger(tick);
          }),
        },
      ],
      scales: { x: { time: false } },
      series: [
        { label: value.xLabel },
        ...value.series.map((series) => ({
          label: series.label,
          stroke: series.stroke,
          fill: series.fill,
          width: series.width ?? 2,
          paths: series.bars ? uPlot.paths.bars?.({ size: [0.9] }) : undefined,
          points: {
            show: series.points ?? false,
            size: 7,
            stroke: series.stroke,
            fill: "#ffffff",
          },
        })),
      ],
    }, data, host);
    let resizeFrame = 0;
    const resize = () => {
      cancelAnimationFrame(resizeFrame);
      resizeFrame = requestAnimationFrame(() => {
        const nextWidth = Math.max(280, Math.floor(host.getBoundingClientRect().width || 640));
        if (nextWidth !== chart.width) chart.setSize({ width: nextWidth, height: 240 });
      });
    };

    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(resize);
    observer?.observe(host);
    return () => {
      observer?.disconnect();
      cancelAnimationFrame(resizeFrame);
      chart.destroy();
    };
  }, [metric.identity.unit, value]);

  return (
    <div
      className="scientific-plot"
      role="img"
      aria-label={value.ariaLabel}
      tabIndex={0}
    >
      <div ref={hostRef} aria-hidden="true" />
    </div>
  );
}

export function DistributionPlot({ metric }: { metric: MetricSummary }) {
  const values = useMemo(() => projection(metric), [metric]);
  if (metric.statistics.distribution.kind === "raw_points") {
    return <RawDistributionPlot metric={metric} />;
  }
  if (values.length === 0) return null;
  return (
    <div className="scientific-plot-grid">
      {values.map((value) => (
        <ScientificPlot key={value.ariaLabel} metric={metric} value={value} />
      ))}
    </div>
  );
}
