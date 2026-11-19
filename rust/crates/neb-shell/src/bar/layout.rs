//! Where the widgets go: three anchored groups, array order inside each, flexible spacers.
//!
//! This is a pure function of the widget list, their measured sizes and the bar's size, so the
//! alignment rules are tested without a window. One rule deserves naming: a `date` entry that
//! directly follows a `clock` entry in the same group is stacked under it, because that is the
//! layout every earlier build drew and because a clock and its date read as one widget.
//!
//! Spacers take the leftover room in their own group. On a bar with only a left group that means
//! the last widget before the spacer is pushed to the right edge; a centre group with a spacer
//! stretches between the side groups instead of overlapping them.

use super::widgets::{Align, WidgetKind, WidgetSpec};

/// Space between two widgets.
pub const GAP: f32 = 8.0;

/// Space between the bar's edge and the first widget of a side group.
pub const PADDING: f32 = 16.0;

/// Smallest width a spacer may ask for, so a crowded group still shows a gap.
pub const SPACER_MIN: f32 = 8.0;

/// What DirectWrite measured for one widget's text.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Measured {
    /// Width of the main line.
    pub width: f32,
    /// Height of the main line.
    pub height: f32,
    /// Width of the second line (a clock's date), or zero.
    pub sub_width: f32,
    /// Height of the second line, or zero.
    pub sub_height: f32,
}

impl Measured {
    /// The height a cell needs: both lines when there is a second one.
    pub fn total_height(&self) -> f32 {
        self.height + self.sub_height
    }

    /// The width a cell needs: the wider of the two lines.
    pub fn total_width(&self) -> f32 {
        self.width.max(self.sub_width)
    }
}

/// The second line of a widget: the date under a clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubPlacement {
    /// Index of the spec that supplies the text: the date entry, or the clock itself.
    pub index: usize,
    /// Left edge in device-independent pixels.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Measured width.
    pub width: f32,
    /// Measured height.
    pub height: f32,
}

/// One widget's final rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Index into the spec list.
    pub index: usize,
    /// What the widget is, for the painter's choice of brush.
    pub kind: WidgetKind,
    /// Which group it belongs to.
    pub align: Align,
    /// Left edge of the main text.
    pub x: f32,
    /// Top edge of the main text.
    pub y: f32,
    /// Measured width of the main text.
    pub width: f32,
    /// Measured height of the main text.
    pub height: f32,
    /// The date under a clock, if it has one.
    pub sub: Option<SubPlacement>,
}

impl Placement {
    /// Whether `(x, y)` lands on the widget, with `margin` pixels of grace on every side.
    pub fn contains(&self, x: f32, y: f32, margin: f32) -> bool {
        let height = self.height + self.sub.map_or(0.0, |sub| sub.height);
        let sub_width = self.sub.map_or(0.0, |sub| sub.width);

        x >= self.x - margin
            && x < self.x + self.width.max(sub_width) + margin
            && y >= self.y - margin
            && y < self.y + height + margin
    }
}

/// A cell before it is placed: one widget, or a clock with its date.
#[derive(Debug)]
struct Cell {
    indices: Vec<usize>,
    align: Align,
    width: f32,
    height: f32,
    spacer: bool,
    measured: Vec<Measured>,
}

/// Lays out `specs` in `size`, with `measured` in the same order.
///
/// Specs that draw nothing (`date` with `showDate: false`) are omitted from the result; spacers
/// affect positions but are not placements. The result is in configuration order.
pub fn place(specs: &[WidgetSpec], measured: &[Measured], size: (f32, f32)) -> Vec<Placement> {
    let cells = cells(specs, measured);
    let mut groups: [Vec<usize>; 3] = [Vec::new(), Vec::new(), Vec::new()];

    for (slot, cell) in cells.iter().enumerate() {
        groups[group_index(cell.align)].push(slot);
    }

    let natural = |slots: &[usize]| -> f32 {
        if slots.is_empty() {
            return 0.0;
        }

        slots.iter().map(|slot| cells[*slot].width).sum::<f32>() + GAP * (slots.len() as f32 - 1.0)
    };

    let has_spacer = |slots: &[usize]| slots.iter().any(|slot| cells[*slot].spacer);

    let natural_left = natural(&groups[0]);
    let natural_centre = natural(&groups[1]);
    let natural_right = natural(&groups[2]);

    let centre_x = (size.0 - natural_centre) / 2.0;
    let right_x = size.0 - PADDING - natural_right;

    let mut placements = Vec::with_capacity(specs.len());

    for (group, slots) in groups.iter().enumerate() {
        if slots.is_empty() {
            continue;
        }

        let natural_width = natural(slots);
        let (start, end) = match group {
            0 => (
                PADDING,
                if !groups[1].is_empty() {
                    centre_x - GAP
                } else if !groups[2].is_empty() {
                    right_x - GAP
                } else {
                    size.0 - PADDING
                },
            ),
            1 if has_spacer(slots) => (
                if groups[0].is_empty() {
                    PADDING
                } else {
                    PADDING + natural_left + GAP
                },
                if groups[2].is_empty() {
                    size.0 - PADDING
                } else {
                    size.0 - PADDING - natural_right - GAP
                },
            ),
            1 => (centre_x, centre_x + natural_width),
            _ if has_spacer(slots) => {
                let left_bound = if !groups[1].is_empty() {
                    centre_x + natural_centre + GAP
                } else if !groups[0].is_empty() {
                    PADDING + natural_left + GAP
                } else {
                    PADDING
                };
                (left_bound, size.0 - PADDING)
            }
            _ => (right_x, right_x + natural_width),
        };

        let budget = natural_width.max(end - start);
        let spacers = slots.iter().filter(|slot| cells[**slot].spacer).count();
        let spacer_width = if spacers == 0 {
            0.0
        } else {
            ((budget - natural_width) / spacers as f32).max(SPACER_MIN)
        };

        let mut cursor = if group == 2 && !has_spacer(slots) {
            right_x
        } else if group == 2 {
            // A spacer in the right group fills from the left bound to the right edge; on a tight
            // bar the natural width still wins and the group stays anchored to the edge.
            end - budget
        } else {
            start
        };

        for (position, slot) in slots.iter().enumerate() {
            let cell = &cells[*slot];

            if cell.spacer {
                cursor += spacer_width + GAP;
                continue;
            }

            let (main_width, main_height, sub_width, sub_height) = match cell.indices.len() {
                1 => {
                    let measured = cell.measured[0];
                    (
                        measured.width,
                        measured.height,
                        measured.sub_width,
                        measured.sub_height,
                    )
                }
                2 => {
                    let clock = cell.measured[0];
                    let date = cell.measured[1];
                    (clock.width, clock.height, date.width, date.height)
                }
                _ => (cell.width, cell.height, 0.0, 0.0),
            };

            let cell_left = cursor;
            let cell_top = ((size.1 - cell.height) / 2.0).max(0.0);
            let main_x = cell_left + (cell.width - main_width) / 2.0;
            let sub = if sub_height > 0.0 {
                Some(SubPlacement {
                    index: *cell.indices.last().unwrap_or(&0),
                    x: cell_left + (cell.width - sub_width) / 2.0,
                    y: cell_top + main_height,
                    width: sub_width,
                    height: sub_height,
                })
            } else {
                None
            };

            placements.push(Placement {
                index: cell.indices[0],
                kind: specs[cell.indices[0]].kind,
                align: cell.align,
                x: main_x,
                y: cell_top,
                width: main_width,
                height: main_height,
                sub,
            });

            if position + 1 < slots.len() {
                cursor += cell.width + GAP;
            }
        }
    }

    placements.sort_by_key(|placement| placement.index);
    placements
}

/// Groups the specs into cells: clocks pair with a directly following date.
fn cells(specs: &[WidgetSpec], measured: &[Measured]) -> Vec<Cell> {
    let mut cells = Vec::with_capacity(specs.len());
    let mut index = 0;

    while index < specs.len() {
        let spec = &specs[index];
        let own = measured.get(index).copied().unwrap_or_default();

        if spec.kind == WidgetKind::Spacer {
            cells.push(Cell {
                indices: vec![index],
                align: spec.align,
                width: 0.0,
                height: 0.0,
                spacer: true,
                measured: vec![own],
            });
            index += 1;
            continue;
        }

        if spec.kind == WidgetKind::Date && !spec.show_date {
            index += 1;
            continue;
        }

        if spec.kind == WidgetKind::Clock
            && let Some(next) = specs.get(index + 1)
            && next.kind == WidgetKind::Date
            && next.align == spec.align
            && next.show_date
        {
            let date = measured.get(index + 1).copied().unwrap_or_default();
            cells.push(Cell {
                indices: vec![index, index + 1],
                align: spec.align,
                width: own.total_width().max(date.total_width()),
                height: own.height + date.height,
                spacer: false,
                measured: vec![own, date],
            });
            index += 2;
            continue;
        }

        cells.push(Cell {
            indices: vec![index],
            align: spec.align,
            width: own.total_width(),
            height: own.total_height(),
            spacer: false,
            measured: vec![own],
        });
        index += 1;
    }

    cells
}

/// `0` left, `1` centre, `2` right.
fn group_index(align: Align) -> usize {
    match align {
        Align::Left => 0,
        Align::Centre => 1,
        Align::Right => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bar::widgets::{Align, WidgetKind};

    fn spec(kind: WidgetKind, align: Align) -> WidgetSpec {
        WidgetSpec {
            kind,
            align,
            format: "HH:mm".to_owned(),
            show_date: true,
            text: kind.name().to_owned(),
            cpu: true,
            ram: true,
        }
    }

    /// A measured box with no second line.
    fn boxed(width: f32, height: f32) -> Measured {
        Measured {
            width,
            height,
            ..Measured::default()
        }
    }

    fn at(placements: &[Placement], index: usize) -> Placement {
        *placements
            .iter()
            .find(|placement| placement.index == index)
            .expect("the widget must be placed")
    }

    #[test]
    fn the_three_groups_are_anchored_to_their_edges() {
        let specs = vec![
            spec(WidgetKind::Launcher, Align::Left),
            spec(WidgetKind::Clock, Align::Centre),
            spec(WidgetKind::Settings, Align::Right),
        ];
        let measured = vec![boxed(16.0, 16.0), boxed(50.0, 20.0), boxed(8.0, 8.0)];

        let placed = place(&specs, &measured, (1_000.0, 40.0));

        assert_eq!(at(&placed, 0).x, PADDING);
        assert_eq!(
            at(&placed, 1).x,
            (1_000.0 - 50.0) / 2.0,
            "the clock is centred"
        );
        assert_eq!(
            at(&placed, 2).x,
            1_000.0 - PADDING - 8.0,
            "the gear is right-aligned"
        );
        assert_eq!(
            placed
                .iter()
                .map(|placement| placement.index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
            "the result stays in configuration order"
        );
    }

    #[test]
    fn widgets_inside_a_group_keep_array_order() {
        let specs = vec![
            spec(WidgetKind::Launcher, Align::Left),
            spec(WidgetKind::Profile, Align::Left),
            spec(WidgetKind::Metrics, Align::Right),
            spec(WidgetKind::Settings, Align::Right),
        ];
        let measured = vec![
            boxed(16.0, 16.0),
            boxed(60.0, 14.0),
            boxed(90.0, 13.0),
            boxed(8.0, 8.0),
        ];

        let placed = place(&specs, &measured, (1_000.0, 40.0));

        assert_eq!(at(&placed, 1).x, PADDING + 16.0 + GAP);
        assert_eq!(at(&placed, 2).x, 1_000.0 - PADDING - 8.0 - GAP - 90.0);
        assert_eq!(at(&placed, 3).x, 1_000.0 - PADDING - 8.0);
    }

    #[test]
    fn a_clock_and_a_date_stack_in_one_cell() {
        let specs = vec![
            spec(WidgetKind::Clock, Align::Centre),
            spec(WidgetKind::Date, Align::Centre),
        ];
        let measured = vec![
            Measured {
                width: 40.0,
                height: 16.0,
                ..Measured::default()
            },
            Measured {
                width: 90.0,
                height: 10.0,
                ..Measured::default()
            },
        ];

        let placed = place(&specs, &measured, (1_000.0, 40.0));

        assert_eq!(placed.len(), 1, "the date is part of the clock's cell");
        let clock = placed[0];
        assert_eq!(clock.kind, WidgetKind::Clock);
        assert_eq!(clock.y, (40.0 - 26.0) / 2.0);
        let sub = clock.sub.expect("the date is the second line");
        assert_eq!(sub.index, 1);
        assert_eq!(sub.y, clock.y + 16.0);
        assert_eq!(sub.width, 90.0);
        assert!((sub.x - (1_000.0 - 90.0) / 2.0).abs() < 0.01);
    }

    #[test]
    fn a_lone_clock_with_its_own_date_is_tall_enough_for_both_lines() {
        let specs = vec![spec(WidgetKind::Clock, Align::Centre)];
        let measured = vec![Measured {
            width: 40.0,
            height: 16.0,
            sub_width: 70.0,
            sub_height: 10.0,
        }];

        let placed = place(&specs, &measured, (800.0, 40.0));

        assert_eq!(placed.len(), 1);
        let sub = placed[0].sub.expect("the clock carries the date");
        assert_eq!(sub.index, 0);
        assert_eq!(sub.width, 70.0);
        assert_eq!(sub.y, placed[0].y + 16.0);
    }

    #[test]
    fn a_hidden_date_is_not_placed() {
        let mut date = spec(WidgetKind::Date, Align::Centre);
        date.show_date = false;
        let specs = vec![spec(WidgetKind::Clock, Align::Centre), date];
        let measured = vec![boxed(40.0, 16.0), boxed(90.0, 10.0)];

        let placed = place(&specs, &measured, (1_000.0, 40.0));

        assert_eq!(placed.len(), 1, "only the clock remains");
        assert!(placed[0].sub.is_none(), "its date was switched off");
    }

    #[test]
    fn a_spacer_pushes_the_group_to_the_opposite_edge() {
        let specs = vec![
            spec(WidgetKind::Text, Align::Left),
            spec(WidgetKind::Spacer, Align::Left),
            spec(WidgetKind::Metrics, Align::Left),
        ];
        let measured = vec![boxed(40.0, 12.0), Measured::default(), boxed(80.0, 12.0)];

        let placed = place(&specs, &measured, (1_000.0, 40.0));

        let text = at(&placed, 0);
        let metrics = at(&placed, 2);
        assert_eq!(text.x, PADDING);
        assert_eq!(
            metrics.x + metrics.width,
            1_000.0 - PADDING,
            "the spacer takes every spare pixel"
        );
    }

    #[test]
    fn a_spacer_between_two_widgets_splits_the_gap() {
        let specs = vec![
            spec(WidgetKind::Text, Align::Left),
            spec(WidgetKind::Spacer, Align::Left),
            spec(WidgetKind::Profile, Align::Left),
        ];
        let measured = vec![boxed(40.0, 12.0), Measured::default(), boxed(80.0, 12.0)];

        let placed = place(&specs, &measured, (500.0, 40.0));

        assert_eq!(at(&placed, 0).x, PADDING);
        assert_eq!(at(&placed, 2).x + 80.0, 500.0 - PADDING);
    }

    #[test]
    fn the_default_layout_never_overlaps_at_a_narrow_width() {
        let specs = vec![
            spec(WidgetKind::Launcher, Align::Left),
            spec(WidgetKind::Profile, Align::Left),
            spec(WidgetKind::Clock, Align::Centre),
            spec(WidgetKind::Date, Align::Centre),
            spec(WidgetKind::Metrics, Align::Right),
            spec(WidgetKind::Settings, Align::Right),
        ];
        let measured = vec![
            boxed(16.0, 16.0),
            boxed(70.0, 14.0),
            boxed(40.0, 16.0),
            boxed(120.0, 10.0),
            boxed(120.0, 13.0),
            boxed(8.0, 8.0),
        ];

        for width in [800.0, 1_366.0, 1_920.0] {
            let placed = place(&specs, &measured, (width, 40.0));
            let mut last_right = f32::MIN;

            for placement in &placed {
                assert!(
                    placement.x >= last_right,
                    "{width}: {:?} at {} overlaps the previous widget ending at {last_right}",
                    placement.kind,
                    placement.x
                );
                last_right = placement.x + placement.width;
            }
        }
    }

    #[test]
    fn a_placement_hit_tests_with_a_margin() {
        let specs = vec![spec(WidgetKind::Launcher, Align::Left)];
        let placed = place(&specs, &[boxed(16.0, 16.0)], (400.0, 40.0));
        let launcher = placed[0];

        assert!(launcher.contains(launcher.x + 4.0, launcher.y + 4.0, 2.0));
        assert!(launcher.contains(launcher.x - 2.0, launcher.y, 2.0));
        assert!(!launcher.contains(launcher.x - 20.0, launcher.y, 2.0));
    }
}
