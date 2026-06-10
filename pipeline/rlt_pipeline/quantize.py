"""Dynamic int8 quantization of the exported GECToR ONNX graph — the combo rten runs fastest
(uint8 activations / int8 weights, ONNX dynamic-quant default). ~4x smaller than fp32."""

from __future__ import annotations

from onnxruntime.quantization import QuantType, quantize_dynamic

from .paths import ONNX_FP32, ONNX_INT8


def main() -> None:
    assert ONNX_FP32.exists(), f"run export first; missing {ONNX_FP32}"
    quantize_dynamic(
        model_input=str(ONNX_FP32),
        model_output=str(ONNX_INT8),
        weight_type=QuantType.QInt8,
        per_channel=False,  # keep ops to the kernels rten covers
    )
    fp32 = ONNX_FP32.stat().st_size / 1e6
    int8 = ONNX_INT8.stat().st_size / 1e6
    print(f"wrote {ONNX_INT8} ({int8:.1f} MB, from {fp32:.1f} MB fp32)")


if __name__ == "__main__":
    main()
