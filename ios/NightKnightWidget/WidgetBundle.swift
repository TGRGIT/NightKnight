import WidgetKit
import SwiftUI

/// Entry point for the WidgetKit extension. Kept in its own file (separate from the
/// widget/view definitions) so the views can be compiled into a unit-test target
/// without dragging in `@main`, which is only valid in an executable/extension.
@main
struct NightKnightWidgetBundle: WidgetBundle {
    var body: some Widget { NightKnightWidget() }
}
