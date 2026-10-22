"""
A helper function to get a default model for quick testing
"""
from omegaconf import open_dict
from hydra import compose, initialize
from hydra.core.global_hydra import GlobalHydra

import torch
from matanyone2.model.matanyone2 import MatAnyone2

def get_matanyone2_model(ckpt_path, device=None) -> MatAnyone2:
    GlobalHydra.instance().clear()
    initialize(version_base='1.3.2', config_path="../config", job_name="eval_our_config")
    cfg = compose(config_name="eval_matanyone_config")
    
    with open_dict(cfg):
        cfg['weights'] = ckpt_path
    with open_dict(cfg.model):
        # Avoid the constructor's torch.hub download; require local backbone weights below.
        cfg.model.pretrained_resnet = False

    # Load the network weights
    if device is not None:
        matanyone2 = MatAnyone2(cfg, single_object=True).to(device).eval()
        model_weights = torch.load(cfg.weights, map_location=device, weights_only=True)
    else:  # if device is not specified, `.cuda()` by default
        matanyone2 = MatAnyone2(cfg, single_object=True).cuda().eval()
        model_weights = torch.load(cfg.weights, weights_only=True)

    backbone_prefixes = (
        "pixel_encoder.conv1.", "pixel_encoder.bn1.", "pixel_encoder.res2.",
        "pixel_encoder.layer2.", "pixel_encoder.layer3.",
        "mask_encoder.conv1.", "mask_encoder.bn1.", "mask_encoder.layer1.",
        "mask_encoder.layer2.", "mask_encoder.layer3.",
    )
    missing_backbone = [
        key for key in matanyone2.state_dict()
        if key.startswith(backbone_prefixes) and key not in model_weights
    ]
    if missing_backbone:
        raise RuntimeError(
            f"MatAnyone2 checkpoint is missing ResNet backbone weights ({', '.join(missing_backbone[:5])}). "
            "Repair the installed MatAnyone2 model before inference."
        )

    matanyone2.load_weights(model_weights)

    return matanyone2
