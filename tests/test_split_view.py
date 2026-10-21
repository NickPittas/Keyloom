import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PySide6.QtCore import QPointF, Qt
from PySide6.QtGui import QImage
from PySide6.QtWidgets import QApplication

from ui.widgets.split_view import SplitViewWidget


def _app() -> QApplication:
    app = QApplication.instance()
    if app is None:
        app = QApplication([])
    return app


def test_display_to_image_rejects_points_outside_visible_left_view():
    _app()
    widget = SplitViewWidget()
    widget.resize(800, 600)

    image = QImage(400, 200, QImage.Format_RGB32)
    widget.set_left_image(image)
    widget.set_right_image(image)
    widget.set_split_enabled(True)

    full_rect = widget._image_rect(image)
    paintable_rect = widget._annotation_paint_rect(image)

    assert paintable_rect is not None
    assert paintable_rect.width() < full_rect.width()

    inside = QPointF(
        paintable_rect.left() + 10.0,
        paintable_rect.center().y(),
    )
    outside_right = QPointF(
        min(full_rect.right() - 1.0, paintable_rect.right() + 10.0),
        paintable_rect.center().y(),
    )
    outside_image = QPointF(full_rect.left() - 5.0, full_rect.top() - 5.0)

    assert widget._display_to_image(inside) is not None
    assert widget._display_to_image(outside_right) is None
    assert widget._display_to_image(outside_image) is None


def _source_tagged(raster_w, raster_h, src_w, src_h):
    image = QImage(raster_w, raster_h, QImage.Format_RGB32)
    image.setText("source_width", str(src_w))
    image.setText("source_height", str(src_h))
    return image


def test_reduced_raster_maps_to_same_source_coordinates_as_full_frame():
    _app()
    full = SplitViewWidget()
    fit = SplitViewWidget()
    for w in (full, fit):
        w.resize(800, 600)
        w.set_view_transform(2.0, QPointF(30.0, -20.0))
    full.set_image(QImage(1600, 800, QImage.Format_RGB32))
    fit.set_image(_source_tagged(800, 400, 1600, 800))

    assert fit._image_rect(fit._single_image) == full._image_rect(full._single_image)
    for pos in (QPointF(420.0, 280.0), QPointF(500.0, 330.0)):
        a, b = full._display_to_image(pos), fit._display_to_image(pos)
        assert a is not None and b is not None
        assert a == b
    assert 0.0 <= fit._display_to_image(QPointF(420.0, 280.0))[0] <= 1599.0


def test_brush_radius_scale_uses_source_width():
    _app()
    widget = SplitViewWidget()
    widget.resize(800, 600)
    widget.set_image(_source_tagged(400, 200, 1600, 800))
    img = widget._single_image
    dest = widget._image_rect(img)
    # 1600 source px -> 800 display px: one source px is 0.5 display px
    assert abs(dest.width() / 1600 - 0.5) < 1e-9


def test_eyedropper_samples_by_fraction_of_raster():
    _app()
    widget = SplitViewWidget()
    widget.resize(800, 600)
    raster = _source_tagged(400, 200, 1600, 800)
    raster.fill(0)
    raster.setPixelColor(399, 0, Qt.red)
    widget.set_image(raster)
    dest = widget._image_rect(raster)
    widget._sample_eyedropper_at(QPointF(dest.right() - 0.5, dest.top() + 0.5))
    assert widget._eyedropper_samples[-1] == (255, 0, 0)


def test_canvas_resized_emits_physical_size_once_per_resize():
    _app()
    widget = SplitViewWidget()
    seen = []
    widget.canvas_resized.connect(lambda w, h: seen.append((w, h)))
    widget.resize(640, 480)
    widget.show()
    seen.clear()
    widget.resize(700, 500)
    dpr = widget.devicePixelRatioF()
    assert seen == [(round(700 * dpr), round(500 * dpr))]
    assert widget.canvas_physical_size() == seen[0]
    widget.close()
