import 'package:flutter/material.dart';
import 'shapes.dart';

typedef ShapeList = List<Shape>;

abstract class Shape {
  double area();
}

mixin Drawable {
  void draw();
}

class Circle extends Shape {
  final double radius;

  Circle(this.radius);

  double area() {
    return 3.14 * radius * radius;
  }
}

class Square extends Shape with Drawable implements Comparable {
  final double side;

  Square(this.side);

  double area() {
    return side * side;
  }
}

extension ShapeInfo on Shape {
  String describe() {
    return "shape";
  }
}

double totalArea(ShapeList shapes) {
  return 0.0;
}
