import contract from "../../ui-contract.json";

/**
 * Height of the window drag band, in CSS pixels.
 *
 * `ui-contract.json` is the only place the number lives: the CSS build injects it into
 * `--caption-band`, this module republishes the property at runtime, and `Neb.App` embeds the same
 * file for `MainWindow.CaptionBandHeight`. The three can therefore never disagree.
 */
export const CAPTION_BAND_HEIGHT: number = contract.captionBandHeight;

/** The CSS custom property the band height is published under. */
export const CAPTION_BAND_PROPERTY = "--caption-band";
